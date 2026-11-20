# CI

`.github/workflows/ci.yml` has five jobs. They run in parallel on every pull
request targeting `main`, including Dependabot and release PRs, and on every
push to `main`:

| Job | Runs |
|---|---|
| `api` | With a pinned PostgreSQL 17 service container and `TEST_DATABASE_URL` pointing at it: `bun run check --filter=@tikkit/api` (`cargo fmt --check`, `cargo clippy -D warnings`) and `bun run test --filter=@tikkit/api` (`cargo test --locked`, including the DB integration tests). |
| `api-release` | `bun run build --filter=@tikkit/api` (`cargo build --release --locked`). |
| `workflows` | `bun run test:e2e:full` against a fresh PostgreSQL database, real API/worker router, built web app, and loopback AI/SMTP stubs. Separate jobs run complete English and German workflows. |
| `web` | For `@tikkit/web` and `@tikkit/api-client`: check (svelte-kit sync + svelte-check), `vite build`, vitest, then `bun run test:e2e` (Playwright against the production build and a stub API). |
| `compose` | Builds both production images, starts PostgreSQL, applies migrations, and checks TLS, readiness, SSR, and security headers on pages, API responses and assets. It verifies rejected hosts and login origins, request-ID replacement, and client-address spoofing protection. It recreates the stack to test both durable volumes, then verifies rejection and readiness reporting for an unknown migration. It runs the reduced [backup and restore drill](backup-restore.md#restore-drill) in its own Compose projects, then runs the full portal workflow through Caddy in a fresh isolated project. That run uses an HTTPS provider stub and verifies data across recreation of the application containers. The `api` job also checks and unit-tests the backup alert rules with `promtool`. |

`.github/workflows/security.yml` has one more job, `audit` (dependency
advisories and the secret scan); see [security-scanning.md](security-scanning.md).

On failure, Playwright traces, screenshots, and the HTML report are uploaded
as the `playwright` artifact. The `workflows` job uploads its traces, videos, screenshots, and HTML report as `playwright-workflows-en` and `playwright-workflows-de`. The Compose run uploads the same artifacts as `playwright-compose-workflows`. See [testing.md](testing.md).

The Bun version comes from `packageManager` in the root `package.json`. Node is
24, and Rust is pinned in the workflow. Actions are pinned to commit SHAs, and
Dependabot updates them.

## Required status checks

The required checks should be **`api`**, **`api-release`**, **`web`**, and **`compose`**. Renaming one
of these jobs breaks branch protection: the old name stays "expected" forever
and every pull request to `main` is blocked.

`audit` is not required. Its advisory steps fail when an advisory is published,
without any change in the repository, and would then block every merge.

The existing `main` protection has `api`, `api-release`, and `web`.
The repository owner needs to add `compose` after GitHub has recorded it
on a run. Open **Settings → Branches → main → Edit → Require status checks
to pass before merging** and select `compose` from GitHub Actions. Keep
the existing three checks.

## Release images

On pushes to `main`, [Release Please in manifest mode](../release-please-config.json)
opens or updates a release PR. The manifest tracks `apps/web` and `apps/api`
independently. Both start at 0.1.0. The API keeps its existing version as
an explicit crate version, and the web package now declares its initial version.
The Cargo workspace plugin updates the root `Cargo.lock` with the API crate.
Other changes outside the component paths, such as a shared client or root
tooling update, do not by themselves trigger a component release. Include
an appropriate Conventional Commit that touches the affected component
when that change needs a release.

After a release PR merges, Release Please creates GitHub releases and tags.
For each tag it creates, the workflow calls **Release images** directly:

- `web-vX.Y.Z` → `ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z`
- `api-vX.Y.Z` → `ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z`

The called workflow runs from `main`, resolves the tag to a commit on
`main`, and runs CI on that exact commit. It publishes a
Linux AMD64 image after CI succeeds and the `release` environment admits
the publishing job. OCI source, revision, and version labels and BuildKit
provenance identify the released revision. Tag pushes do not invoke the
image workflow. Use an image digest for deployments that pin an artifact.

If CI or publication failed before the registry accepted the image, use
manual dispatch on **main**, supplying the component and existing Git tag:

```sh
gh workflow run release-images.yml --ref main -f component=web -f tag=web-vX.Y.Z
```

The same CI and environment rules apply. The publisher refuses an existing
GHCR tag and aborts when the registry cannot confirm absence. Per-tag
concurrency serializes these workflow runs. Keep package writes limited
to this release path. Registry tags are mutable to other authorized
publishers, so manual retries are for missing images only.

### Repository owner setup

Configure these settings before enabling releases:

1. Under **Settings → Environments**, create `release`. For **Deployment
   branches and tags**, choose **Selected branches and tags**, add branch
   `main`, and add no tag rules. Keep `main` protected. Add required release
   reviewers and disable administrator bypass if releases should require
   manual approval. Both automatic and manual publication use this environment.
   See [GitHub's environment settings](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments).
2. Create and install a dedicated GitHub App on this repository only, with
   repository permissions **Contents**, **Pull requests**, and **Issues**
   set to read and write. In the `release` environment, put the client ID
   in variable `RELEASE_APP_CLIENT_ID` and the private key in secret
   `RELEASE_APP_PRIVATE_KEY`. Release Please uses a short-lived installation
   token so its release PRs trigger regular CI. Keep this key in the
   environment and rotate it if compromised.
3. Under **Settings → Rules → Rulesets**, create an **Active** tag ruleset
   for both `web-v*` and `api-v*` with **Restrict creations** and the
   dedicated GitHub App as the only bypass actor. Create a second **Active**
   tag ruleset with the same patterns, **Restrict updates** and **Restrict
   deletions**, and an empty bypass list. Splitting the rules lets the App
   create release tags while the update and deletion rules still apply to it.
   See [GitHub's tag rulesets](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/creating-rulesets-for-a-repository).
4. In the two GHCR package settings, grant this repository Actions write
   access if needed and remove other package writers. Make the packages
   public for anonymous pulls if intended. The publisher uses the short-lived
   `GITHUB_TOKEN`. GHCR grants that access to the repository, so the
   environment protects this job while repository writers remain trusted to
   author workflows. Keep write access limited to release maintainers and
   have other contributors use forks.

The first hosted release is still pending. Release PR automation, tag rules,
environment enforcement, and first authenticated GHCR publication require
the repository settings and a GitHub Actions run. The Dockerfiles and Compose
smoke path can be checked locally.
