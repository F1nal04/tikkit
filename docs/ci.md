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

## Dependency updates (`.github/dependabot.yml`)

Weekly (Monday) Dependabot version updates, at most 5 open PRs per ecosystem:

| Ecosystem | Directory | Groups | Commit prefix |
| --- | --- | --- | --- |
| `bun` | `/` (workspace + `bun.lock`) | `tanstack` (`@tanstack/*`), `bun-dependencies` (everything else) | `chore(deps)` |
| `cargo` | `/` (workspace + shared `Cargo.lock`) | `cargo-dependencies` | `chore(deps)` |
| `github-actions` | `/` (`.github/workflows`) | `github-actions` | `chore(ci)` |

Dependabot regenerates `bun.lock` / `Cargo.lock` in its PRs and updates SHA
pins together with their `# vX.Y.Z` comments. Its PRs run the same `checks`
job (and `Security` workflow) as human PRs; nothing skips bot actors. Two
things are not tracked and are bumped by hand: `RUST_TOOLCHAIN` in both
workflows, and the `node-version` / `GITLEAKS_VERSION` / `cargo-deny@x.y.z`
pins inside step inputs.

Dependabot version updates and Dependabot alerts are free for public and
private repositories. Enable alerts and security updates under **Settings →
Code security**; the `Security` workflow below is the runnable check that does
not depend on those settings.

## `Security` workflow (`.github/workflows/security.yml`)

Runs on pull requests to `main`, pushes to `main`, weekly (Monday 05:17 UTC,
so new advisories surface without a code change) and on manual dispatch. One
`security` job, `contents: read`, same concurrency scheme as CI. It is a
separate workflow so the scheduled run does not re-run the full build; it can
be added as a second required check (`security`) if desired.

### Rust advisories

`cargo deny --locked check advisories` (cargo-deny pinned in the workflow,
installed with `taiki-e/install-action`), configured by `deny.toml`:

- Any RustSec **vulnerability** advisory fails, regardless of severity score.
- **Unmaintained** and **unsound** advisories fail for direct workspace
  dependencies and warn for transitive ones.
- **Yanked** crates in `Cargo.lock` fail.

Locally: `cargo install cargo-deny --locked` (or download a release), then
`cargo deny --locked check advisories`.

### JavaScript vulnerabilities

`bun audit --audit-level=high` after `bun install --frozen-lockfile
--ignore-scripts`. **High** and **critical** advisories fail the job; low and
moderate are not printed and do not fail it (run plain `bun audit` to see
them, and fix them in routine dependency updates).

### Secret scanning

`gitleaks git --redact` over the full history with `.gitleaks.toml`, which
extends the upstream default rules and adds Tikkit rules for OpenAI
(`sk-proj-`, `sk-svcacct-`, `sk-admin-`, legacy `sk-…`), Anthropic
(`sk-ant-…`) and Google (`AIza…`) keys. `--redact` keeps matched values out
of logs and reports. The gitleaks binary is a pinned release verified by
SHA-256 (the gitleaks GitHub Action is not used).

`scripts/verify-secret-scan.sh` is the safe fixture: it builds obviously fake
keys (`…TIKKITFAKE…` plus random characters) at runtime in a temporary
directory outside the repository, scans them with the repository config, and
fails unless every provider rule fires and no fake key is printed. CI runs it
after the real scan; run it locally with gitleaks on `PATH`. Nothing that
matches the rules is committed, so the repository scan stays clean.

Locally: `gitleaks git --redact --verbose --config .gitleaks.toml .`

GitHub's own secret scanning and push protection are free for public
repositories but need GitHub Secret Protection for private ones. The gitleaks
step is the plan-independent check; enable GitHub's scanning as well when it
is available.

If a real secret is found: revoke/rotate it first, then remove it. Rewriting
history does not un-leak a pushed key.

### Severity and exception policy

- Fix rather than ignore: update the dependency (or the lockfile) in the PR
  that turns the check red, or in a dedicated `chore(deps)` PR.
- An exception needs a written justification (why it is not exploitable here
  or why no fix exists), a tracking issue, and a review date no more than 90
  days out. Remove it when the date passes or a fix ships.
- Rust: add `{ id = "RUSTSEC-YYYY-NNNN", reason = "…; tracked in #N; review by YYYY-MM-DD" }`
  to `ignore` in `deny.toml`.
- JavaScript: add `--ignore=GHSA-xxxx-xxxx-xxxx` to the `bun audit` step in
  `security.yml` with a comment line above it carrying the same
  justification, issue and review date.
- Secrets: only for confirmed false positives (never for real or test
  credentials). Add the finding's `Fingerprint` (from `gitleaks … --report-path`)
  to `.gitleaksignore` with a preceding `# reason; tracked in #N; review by YYYY-MM-DD`
  comment, or prefer narrowing the content (for example splitting a
  fixture string) so it no longer matches.
