# Continuous integration

## `CI` workflow (`.github/workflows/ci.yml`)

One job, `checks`, on `ubuntu-latest`. It runs for every pull request that
targets `main` (including Dependabot and release-please PRs; there is no bot
skip) and for every push to `main`.

- Permissions: `contents: read` only. Pull requests from forks and bots get a
  read-only token and no repository secrets; the job needs none.
- Concurrency: `${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}`
  with `cancel-in-progress: true`. A new push to a PR cancels that PR's older
  run; other PRs and `main` are unaffected.
- No cache actions, no matrix, no extra jobs. Every run starts from a clean
  runner; Turbo reuses the frontend build only within the job (the web
  `check` task depends on `build` to generate route types).

Steps, in order:

| Step | What | Pin |
| --- | --- | --- |
| Checkout | `actions/checkout` | full SHA, `# vX.Y.Z` comment |
| Bun | `oven-sh/setup-bun` with `bun-version-file: package.json` | version from root `packageManager` |
| Node | `actions/setup-node`, `node-version: 22.22.0` | satisfies `engines.node` (`>=22.12.0`) |
| Rust | `rustup toolchain install $RUST_TOOLCHAIN` with rustfmt + clippy | `RUST_TOOLCHAIN` in the workflow `env` |
| Install | `bun install --frozen-lockfile` | fails on a stale `bun.lock` |
| Check | `bun run check` | tsc, `cargo fmt --check`, `cargo clippy --locked -D warnings` |
| Build | `bun run build` | Vite/Nitro build, `cargo build --release --locked` |
| Test | `bun run test` | `cargo test --locked`, `TEST_DATABASE_URL` set |
| Browser tests | `bunx playwright install --with-deps chromium`, then `bun run test:e2e` | `E2E_DATABASE_URL` set |
| Artifacts | `actions/upload-artifact` on failure | `apps/web/test-results/`, `apps/web/playwright-report/` |

A PostgreSQL 16 service container (`tikkit`/`tikkit`, port 5432, health
checked) is available to the test steps at
`postgres://tikkit:tikkit@127.0.0.1:5432/postgres`.

The Rust toolchain is installed with the runner's preinstalled `rustup` rather
than a third-party action, so there is no extra action to pin. Bump
`RUST_TOOLCHAIN` deliberately (it is not tracked by Dependabot) and keep it in
step with the local toolchain you verify with. The Cargo scripts in
`apps/api/package.json` keep `--locked`, so a `Cargo.lock` that does not match
`Cargo.toml` fails the job.

### Required status check

The stable required check is **`checks`** (the job id; GitHub shows it as
`CI / checks`). Do not rename the job without updating branch protection.

Branch protection must be configured by a repository admin; automation in this
repository has no admin rights. Manual setup:

1. Repository **Settings → Rules → Rulesets → New ruleset → New branch
   ruleset** (or **Settings → Branches → Add branch protection rule** for the
   classic UI).
2. Name it `main`, set **Enforcement status** to *Active*, and target the
   default branch (`main`).
3. Enable **Require a pull request before merging**.
4. Enable **Require status checks to pass**, enable **Require branches to be
   up to date before merging**, and add the check `checks` (source: GitHub
   Actions). The check appears in the picker once the workflow has run at
   least once on a PR.
5. Enable **Block force pushes** and **Restrict deletions**.
6. Leave the bypass list empty so bots (Dependabot, release-please) and
   admins go through the same check.

Rulesets and classic branch protection are available for public repositories
on GitHub Free; private repositories need a paid plan. Without them, treat a
red `checks` run as merge-blocking by convention.

### Proving the gate

On a throwaway branch, open a PR with one deliberate failure at a time and
confirm `checks` goes red, then revert and confirm it goes green:

- TypeScript: assign a string to a `number` in `apps/web/src`.
- rustfmt: mis-indent a line in `apps/api/src/lib.rs`.
- Clippy: add `let x = 1; let _ = x.clone();` (`clone_on_copy`).
- Test: change an expected value in an API test.
- Frozen lockfile: bump a version in `apps/web/package.json` without running
  `bun install` (or edit `Cargo.toml` without updating `Cargo.lock`).

Push two commits in quick succession to see the first run cancelled.
