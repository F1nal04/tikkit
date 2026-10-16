# Dependency and secret scanning

## Automation

- **Dependabot** (`.github/dependabot.yml`): weekly update PRs for Bun (root
  `bun.lock`), Cargo (the root workspace and `Cargo.lock`), and the GitHub
  Actions SHA pins. Updates are grouped like Bookr's: one group for the
  Svelte/Vite toolchain, one for everything else, and one for actions. Dependabot
  PRs run the same required `checks` job as human PRs.
- **`Security` workflow** (`.github/workflows/security.yml`): runs on PRs, on
  pushes to `main`, and every Monday. It runs:
  - `bun audit --audit-level=high` for JavaScript advisories.
  - `cargo audit` for RustSec advisories against `Cargo.lock`.
  - gitleaks over the git history in the PR or push range, with `--redact` so
    findings never print the secret. The default rules include AI provider keys
    (OpenAI, Anthropic, and others). Test fixtures are scanned like any other file.
    No license is needed because the repo belongs to a personal account. If it
    moves to an organization, add a free `GITLEAKS_LICENSE` secret, or replace
    the step with `docker run zricethezav/gitleaks git . --redact`.
- **GitHub settings**, enabled 2026-10-16 through the API: Dependabot alerts,
  Dependabot security updates, secret scanning, and push protection. The repo is
  public, so these are free.

## Severity policy

- JavaScript: high and critical advisories fail the workflow. Low and moderate
  ones are fixed through the normal weekly updates.
- Rust: every RustSec vulnerability fails the workflow. Most have no CVSS score,
  so there is nothing to filter on. Unmaintained and yanked crates only warn.
- Secrets: any finding fails the workflow. Rotate the credential first, then
  remove it from the code.

## Exceptions

An exception needs a GitHub issue that explains why the advisory doesn't apply
or can't be fixed yet. Remove the exception when the issue closes.

- Rust: add the ID to `.cargo/audit.toml`, with the issue link as a comment:
  ```toml
  [advisories]
  ignore = ["RUSTSEC-2026-0000"] # https://github.com/F1nal04/tikkit/issues/N
  ```
- JavaScript: add `--ignore=GHSA-xxxx-xxxx-xxxx` to the `bun audit` step, with
  the issue link in a comment.
- Secrets: for a confirmed false positive, add a `.gitleaks.toml` allowlist
  entry for that exact path or pattern, with the issue link. Never allowlist a
  whole fixture directory.

Current exceptions:

- `RUSTSEC-2023-0071` (`rsa` 0.9, Marvin timing attack, no fix released): locked
  only as an optional dependency of `sqlx-mysql` through `sqlx-macros-core`. It is
  never compiled for the Postgres-only API (`cargo tree -e all -i rsa --target all`
  is empty). Remove the exception when sqlx drops it from the lockfile or `rsa`
  publishes a fix. No tracking issue filed yet.
