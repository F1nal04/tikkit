# Release validation: first single-organization release

The record for [issue #52](https://github.com/F1nal04/tikkit/issues/52): what was
run against the complete Milestone 4 stack, what it showed, what could not be
checked on this machine, and the release recommendation. Nothing here is taken
from pull-request descriptions; every result below comes from a command run for
this record. Scratch logs were kept outside the repository and are not
committed; the relevant excerpts are quoted here. All ticket content was
synthetic.

## Result in one paragraph

**Recommendation: go, with conditions.** No functional defect was found; two
Admin-report latency targets are missed under the capped reference load
(F-6, already recorded in the performance baseline). Of the 20
acceptance criteria of #52, **19 pass** and **1 is not verifiable here** (CI on
the release commit and artifact traceability: the release commit on `main` and
the published images do not exist yet). No criterion failed. The conditions are
the repository settings in [Repository settings](#repository-settings-the-owner-must-apply)
and the items in [Not verifiable on this machine](#not-verifiable-on-this-machine);
the remaining risks are listed with an owner in
[Known limitations and remaining risks](#known-limitations-and-remaining-risks).

## Release candidate

| | |
| --- | --- |
| Candidate commit | `c14af5a4431ab24bce835ab6bb2a66c1dd20929d` (tip of `docs/guides`, PR #129) |
| Base | `origin/main` at `effb6b276a128db83afd8a52387a48daba419370` |
| Stack | 148 commits in 12 pull requests, merged bottom-up: #117, #118, #120, #121, #122, #123, #124, #125, #126, #127, #128, #129 (GitHub stack #119) |
| Validated | 2026-11-19, 22:40 to 23:30 UTC |
| This record | Branch `chore/release-validation` on top of the candidate. It adds this page, two links and a one-line documentation correction; no product code changed. |

The images under test were built from the candidate working tree with the
Dockerfiles in the repository (`docker build -f apps/api/Dockerfile`,
`-f apps/web/Dockerfile`, `-f ops/backup/Dockerfile`). They are local builds
without OCI revision labels; see criterion 1.

## Environment

| | |
| --- | --- |
| Host | Apple M4 Pro developer workstation, 14 cores, 48 GiB RAM, macOS 27.0.1 (arm64). Shared with other work during the run. |
| Docker | Docker Desktop, Engine 29.8.2, Compose v5.6.0. The Docker VM had 14 vCPU and 7.75 GiB RAM. Images ran as `linux/arm64`. |
| Toolchain | Bun 1.4.3, Node 24.13.0, Rust 1.99.0, cargo-audit 0.22.2, Playwright Chromium from the repository's lockfile |
| Database for the test suites | PostgreSQL 17 (`docker-compose.dev.yml`), one database per test |
| Compose projects used | `tikkit-m4-52` (install guide, ports 18944/18980), `tikkit-m4-52-restore` (18945/18981), `tikkit-m4-52-ai` (18946/18982), `tikkit-m4-52-drill` and `-drill-restore` (18950/18951), plus the fixed projects of the repository's own scripts: `tikkit-m4-48` (`ops/compose/workflows.sh`) and `tikkit-m4-50` (`ops/load`). All were removed afterwards with their volumes, networks and locally built images. |

Published release images are `linux/amd64`. This run executed ARM64 builds of
the same Dockerfiles; the AMD64 path is exercised only by the `compose` CI job.

## Whole-stack verification at the candidate

| Check | Command | Result |
| --- | --- | --- |
| Static checks, uncached | `bun run check --force` | Pass. 3 tasks, 0 cached (svelte-check, `cargo fmt --check`, `cargo clippy -D warnings`). |
| Production builds | `bun run build --force` | Pass. `vite build` and `cargo build --release --locked`. |
| Unit and integration tests | `TEST_DATABASE_URL=… bun run test --force` | Pass. Rust: **440 passed, 0 failed, 0 ignored** in 36 test binaries, none skipped for a missing database. Web: **189 passed** in 34 files. API client: **6 passed**. |
| Browser suite, stub API | `CI=1 bun run test:e2e --force` under the shared lock | Pass. **156 passed (3.4m)**. Includes the axe scan (`a11y.spec.ts`), keyboard and failure-recovery specs. |
| Browser workflow, real router, English | `CI=1 bun run test:e2e:full` | Pass. **1 passed (44.4s)**. |
| Browser workflow, real router, German | `WORKFLOW_SETUP_LANGUAGE=de CI=1 bun run test:e2e:full` | Pass. **1 passed (28.9s)**. |
| Browser workflow, production images through Caddy, English | `ops/compose/workflows.sh` | Pass. **1 passed (47.7s)**. |
| Same, German | `WORKFLOW_SETUP_LANGUAGE=de ops/compose/workflows.sh` | Pass. **1 passed (1.1m)**. |
| Rust advisories | `cargo audit` | Pass. 340 crates, no finding beyond the two documented exceptions in `.cargo/audit.toml`. |
| JavaScript advisories | `bun audit --audit-level=high --ignore=GHSA-fc5q-39xc-vjpf` | Pass. "No vulnerabilities found (checked 184 packages)". The same command **without** the documented `--ignore` also reports nothing; see finding F-2. |
| Secret scan | gitleaks (Docker image) over `origin/main..c14af5a` with `--redact` | Pass. "147 commits scanned … no leaks found". |
| Workflow lint | `actionlint` (Docker image) | Pass, no output. |
| Alert rules | `promtool check rules` and `promtool test rules` with the pinned image from `ci.yml` | Pass. 11 + 4 rules found, both test files `SUCCESS`. |
| OpenAPI drift | `cargo run --locked -p tikkit-api --bin openapi` compared with `apps/api/openapi.json` | No drift (byte-identical). |
| API client drift | `bun run --cwd packages/api-client generate` | No change in the working tree. |

### Stack hygiene

| Check | Result |
| --- | --- |
| History | `git log --merges origin/main..HEAD` is empty: 148 commits, linear. |
| Branch ancestry | Each of the 12 branches is an ancestor of the next, starting from `origin/main` (checked with `git merge-base --is-ancestor`). |
| Trailers | No commit in the range carries `Co-Authored-By` in any spelling (`grep -ci` over all commit bodies: 0). |
| Migrations | Only additions: `0024_admin_submissions.sql`, `0025_request_correlation.sql`, `0026_audit_events.sql`, `0028_data_retention.sql`. `git diff --name-status origin/main..HEAD -- apps/api/migrations` shows no modified file, so every migration that exists on `main` is byte-identical. Numbers follow the reserved plan (0027 and 0029 to 0031 unused). |
| CI per pull request | `gh pr checks` for all 12: every reported job passes. #117 to #123: `api`, `api-release`, `web`, `audit`. #124 and #125: plus `compose`. #126 to #129: plus `workflows (en)` and `workflows (de)`. |
| Commit messages | Conventional commit types throughout, with an app scope where a commit belongs to one app (`feat(api)`, `fix(web)`, `test(ops)`; unscoped `docs`, `ci`, `chore`). |

## Checklist of issue #52

One row per acceptance criterion, in the order of the issue. "Compose" means the
production images behind Caddy; "real router" means the Playwright workflow
against the real Axum router and PostgreSQL in one process; "integration" means
the Rust tests in `apps/api/tests/`. Details and excerpts are in the sections
after the table.

| # | Criterion (short) | What was run | Result | Evidence |
| --- | --- | --- | --- | --- |
| 1 | Required CI green on the release commit; artifacts and images traceable to it | `gh pr checks` for the 12 pull requests; static read of the release workflows; `gh release list`, `git ls-remote --tags` | **Not verifiable here** | CI is green on the candidate (`c14af5a`, PR #129: `api`, `api-release`, `web`, `compose`, `workflows (en)`, `workflows (de)`, `audit`). The release commit is the commit on `main` after the stack merges and does not exist yet. No release, tag or registry image exists, and the local builds carry no revision label. The release workflow adds `org.opencontainers.image.revision` and provenance only when it publishes. |
| 2 | All defining AI workflows and full operator resolution | Compose run for OpenAI, Gemini and Anthropic (this record's own driver), `ops/compose/workflows.sh` in English and German, real-router workflows, integration suite | **Pass** | [Provider matrix](#provider--scenario--layer-matrix): every defining workflow of this criterion passed on Compose for all three providers; the matrix names what ran on which layer. Operator resolution: [deployment validation](#clean-production-style-deployment). |
| 3 | Core service desk verified; SLA deferred; knowledge base optional | Install-guide deployment with communication, categories, routing, teams and administration; code and Compose search for SLA and knowledge-base parts | **Pass** | 219 scripted checks passed on the deployed stack. No SLA or knowledge-base table, route, setting, service or dependency exists ([scope check](#scope-and-dependencies)). |
| 4 | Portal-only tickets and replies; outbound SMTP is Admin-configurable; no inbound path; SSO not required | Deployment run without SMTP; `ops/load` smoke and recovery with a TLS capture relay configured through the Admin API; integration tests; route inventory | **Pass** | Ticketing completed with `{"connection":null}`. Smoke: 10 ticket emails sent through the relay, 0 duplicate notifications; recovery: 147 accepted, 0 duplicates. `there_is_no_inbound_email_or_webhook_channel`, `starttls_delivers_test_invitation_and_recovery_mail`, `implicit_tls_and_german_test_mail_work` pass. No IMAP, POP3, webhook, OIDC, SAML or LDAP code or dependency. A real relay was not used ([not verifiable](#not-verifiable-on-this-machine)). |
| 5 | Security, accessibility, performance, retention and the restore drill have recorded results with owned risks | Proxy and origin-policy probes, audits, axe scan, load smoke and profile, retention flow, restore drill | **Pass** | The criterion asks for recorded results and owned risks, and both exist; two Admin-report latency targets are missed and recorded as F-6. Results: [proxy](#reverse-proxy-through-the-running-stack), [accessibility](#accessibility), [performance](#load-and-performance), [retention](#restore-mode-retention-audit-and-rate-limits), [restore drill](#backup-and-isolated-restore). Risks: [table](#known-limitations-and-remaining-risks). |
| 6 | Documentation and runbooks match the release; no production secret in the repository | Install guide followed literally; 11 documentation claims and 3 troubleshooting symptoms checked on the stack; gitleaks | **Pass** | [Documentation against the product](#documentation-against-the-product). One wording gap found and corrected on this branch (F-1). Tracked env files are `.env.example` (development placeholders) and `.env.production.example` (empty secret values). |
| 7 | Human-closed tickets not reopenable by default; Admin can enable and disable; AI-solved stays final in both states | Deployment run (browser and API); Compose AI run for the three providers | **Pass** | Default: requester, assignee and Admin `403`. Toggle set by an Admin in the settings page: requester reopens in the browser, assignee reopens through the API, outsiders `404`/`403`. Toggle off again: `403` for all three and the button is gone. AI-solved: seven actors refused with the toggle off and on, for each provider. |
| 8 | Different categories enter one shared general queue; manual transfer keeps identity, reference, intake and timestamps | Deployment run | **Pass** | Hardware and Software tickets visible to seven accounts (two Operators, a Specialist, two Team Leads, an Operator without a team, the Admin), `team` and `assignee` null. After dispatch: same ID, `#1 -> #1`, intake snapshot identical, `submit@2026-11-19T22:56:44.461776Z` unchanged, ticket count unchanged, status still `queued`, the other ticket still in the general queue. |
| 9 | First Admin exactly once; invitation defaults and preselected roles; single use; replay and public signup refused; role changes Admin-controlled; five roles against the matrix | Deployment run; `authz_matrix` integration tests | **Pass** | Fresh database: 0 accounts. Setup in the browser, second attempt `409 setup_already_completed` signed out and signed in, 1 account. Default invitation `user`; `operator`, `specialist`, `team_lead`, `admin` applied; replay `410 invitation_invalid`; smuggled `role` `422 unknown_field`; revoked link `410`; five signup-shaped requests create nothing. Non-Admins get `403` on invitations and role changes. `every_route_has_a_matrix_row`, `every_route_allows_exactly_the_expected_callers`, `matrix_matches_spec` pass. |
| 10 | Operator/Specialist parity; Team Lead scope; denied cross-team and global operations; Admin organization-wide; dispatch leaves the ticket unassigned | Deployment run | **Pass** | Identical `staff_access` for Operator and Specialist on the same unclaimed team ticket; the Specialist claims, starts, notes, replies, re-prioritises, waits, resumes and proposes. Team Lead assigns and reassigns in the own team, `422` for a member of another team, `404` on another team's ticket, `403` on 11 Admin operations. Admin reads and assigns across teams. Dispatch result: `"assignee_display_name":null`. |
| 11 | One team and one led team; separate Admin identity; through UI, API, invitations, role changes and concurrent writes | Deployment run (API), stub browser suite (`admin.spec.ts`), integration tests for the concurrent cases | **Pass** | Team without a lead, with an Operator, an Admin, a User or an already-leading lead: `422`. Active team losing its lead `422`; moving the sole lead `409 active_team_lead`; Admin joining a team, User joining a team, promotion to Admin with membership: `422`. Admin cannot claim (`403`) or be assigned (`422`). Concurrent writes ran in process only: `concurrent_membership_and_admin_promotion_cannot_both_commit`, `competing_teams_cannot_promote_the_same_replacement_lead`, `competing_team_creations_cannot_share_a_lead_and_restore_requires_an_eligible_lead` pass. |
| 12 | Working optional AI for OpenAI, Gemini and Anthropic: contract tests and the shared ticket workflow | Contract tests, integration workflow per provider, Compose run per provider | **Pass** | `openai_request_shape`, `gemini_request_shape`, `anthropic_request_shape`, `success_is_normalized_with_reported_usage`, `malformed_payloads_are_rejected`, `{openai,gemini,anthropic}_intake_decisions_and_failure_handoff` pass. Compose: 4 authenticated calls per provider to its official host and path, 12 in total. No real provider was called ([not verifiable](#not-verifiable-on-this-machine)). |
| 13 | AI without a knowledge base; category, description, conditional answers, authorized screenshots; text and image prompt safety | Integration workflow per provider; Compose run per provider; code search | **Pass** | Integration, all three providers: the request carries category, the visible conditional answer and one image, and none of twelve private markers (other tickets, hidden answers, notes, requester identity); instructions and untrusted text are separate; no `tools` key. Compose, all three: one PNG image part, `tools: false`, no portal URL. No retrieval or article code exists. Whether a real model obeys the instructions cannot be shown with a stub. |
| 14 | No dependency on #29 or #30; no SLA configuration, scheduler, dashboard or reminder; the confirmation auto-close worker is required | Issue states, code and Compose search, worker metrics, timeout run on Compose | **Pass** | #29 and #30 are open; no code, migration, workflow or Compose file refers to them or contains SLA logic. Compose services: `postgres`, `api`, `web`, `proxy`. Worker freshness metric present for `auto_close`; an overdue proposal closed within 2 s on the deployed stack. |
| 15 | Clean self-hosted Compose installation: migrations, first Admin, single-use invitations, persisted attachments, workflows, restart persistence, isolated restore; no hosted platform, object storage, knowledge base or SLA scheduler | `docs/guides/install.md` followed step by step; backup and restore sections of the same guide; restore drill | **Pass** | [Deployment validation](#clean-production-style-deployment), [persistence](#persistence-across-restart-and-recreation), [restore](#backup-and-isolated-restore). |
| 16 | One shared form with conditional program details; screenshot understanding with deterministic multimodal fixtures for the three providers; text-only works; image and model failures hand off without exposing private context | Deployment run (conditional field), integration workflow per provider, Compose run per provider | **Pass** | Conditional `Program` field answered by choice (browser, German) and by free text (API). Integration, per provider: the decoded image part equals the fixture pixel for pixel; `vision_failed` and `malformed_output` hand off once; no private marker in any request. Compose, per provider: text-only request with 0 image parts, screenshot request with 1 PNG part, `401` and malformed output each hand off once with `suggestion: null`. A real model's understanding of a screenshot is not verifiable here. |
| 17 | Requester-confirmed closure end to end | Deployment run, browser in English and German | **Pass** | Proposal: `awaiting_confirmation`, report `initial_requester_confirmed: 0`. Five other accounts (two Operators, a Specialist, a Team Lead, the Admin) and another requester refused on `accept_resolution`; wrong proposal version `409`. Reject: `in_progress`, same team and assignee, no reopen event. Accept: `closed`/`requester_confirmed`; replays `409` and no further event. Timeout: `closed`/`confirmation_timeout`. |
| 18 | #55: auto-close works in Compose without an SLA subsystem and is distinguished in UI, notifications, audit and statistics; cannot mark AI-pending records solved or bypass reopening | Deployment run; recovery drill with two API replicas; integration tests | **Pass** | Compose: deadline snapshot 168 h, then 2 h after the Admin changed it, unchanged by a later settings change; closure reason `confirmation_timeout`; page text "Closed automatically: no response before the confirmation deadline"; audit `ticket_confirmation_timeout:auto_close`; report `initial_requester_confirmed: 2`, `initial_confirmation_timeout: 1`; late accept `409`; reopen `403`. Two replicas: 10 timed out, 4 requester-confirmed, no double closure. The notification text and the AI-pending guard were checked in process only: `overdue_proposal_closes_once_with_the_timeout_reason`, `nothing_else_is_ever_auto_closed`. |
| 19 | English and German critical workflows, system email templates and accessibility text; persisted language; single-solution AI across language changes | Real-router and Compose workflows in both languages; deployment run with a German requester; catalog comparison; axe scan in both languages; mail integration tests | **Pass** | Both workflow suites pass in `en` and `de`. German decision flow in the browser on the deployed stack. 1,098 interface strings: 0 missing, 0 blank in German. Account language persists and drives server rendering. The suggestion keeps its own `lang` when the interface language changes and no second provider call follows. `public_reply_reaches_the_requester_in_their_language_without_content`, `implicit_tls_and_german_test_mail_work` pass. |
| 20 | SvelteKit migration complete: checks and tests, SSR and hydration without cross-user leakage, deep links and client navigation in the Node-adapter deployment, performance baseline recorded | `bun run check`, web tests, both browser suites, SSR and navigation probes on the deployed stack, load smoke and profile, bundle budget | **Pass** | [SSR and navigation](#ssr-hydration-and-navigation). Client JavaScript 172,604 bytes gzip against the 204,800 budget. [Performance](#load-and-performance). |

**Totals: 19 pass, 0 fail, 1 not verifiable here** (criterion 1).

## Provider × scenario × layer matrix

Layers:

- **Compose**: production API and web images behind Caddy, PostgreSQL, real
  workers. The API keeps the official provider URLs and its public-address
  policy; on an internal Docker bridge the official hostnames resolve to a
  deterministic HTTPS stub whose CA only that API container trusts. Two
  runs: the repository's `ops/compose/workflows.sh` (OpenAI only), and a
  scratch overlay written for this record with the same technique that
  answers for `api.openai.com`, `generativelanguage.googleapis.com` and
  `api.anthropic.com` and logs request metadata. The scratch overlay and stub
  are not committed; production code has no configurable provider URL.
- **Real router**: `bun run test:e2e:full`, Playwright against the real router,
  PostgreSQL and workers with the in-process provider mock. OpenAI only.
- **Integration**: `apps/api/tests/ai_attempts.rs` and neighbours, the router
  driven in process against PostgreSQL and the loopback mock.
- **Contract**: `apps/api/tests/ai_providers.rs`, the adapters alone.

`O`, `G`, `A` = OpenAI, Gemini, Anthropic. An entry lists the providers that
were exercised for that scenario on that layer; "none" means it did not run
there.

| Scenario | Compose | Real router | Integration | Contract |
| --- | --- | --- | --- | --- |
| AI-disabled intake reaches the queue with zero provider calls | provider-independent: run twice | once | `submission_freezes_intake_and_routes_once_with_ai_disabled` | not applicable |
| Mandatory AI for a non-exempt category | O G A | O | O G A | not applicable |
| Unauthorized skip rejected in the API | O G A (`403` on submission and on `skip_ai`) | O | O | not applicable |
| Unauthorized skip not offered in the form | O G A | O | not applicable (stub browser suite: "mandatory AI by default") | not applicable |
| Admin-enabled requester skip, in the form and the API, zero calls | O G A | none | O (`a_requester_skip_needs_the_admin_toggle`) | not applicable |
| Skip refused again after the toggle is turned off | O G A | none | O (`removed_key_and_revoked_skip_use_current_admin_policy`) | not applicable |
| Category exemption with zero provider calls | O G A (stub log and `ai_usage` unchanged, no attempt row) | none | O (`an_exempt_category_makes_no_provider_call_even_mid_flight`) | not applicable |
| Text-only submission, one suggestion | O G A (0 image parts) | O | O G A | O G A |
| Screenshot submission as a real image input | O G A (1 PNG part, no portal URL) | none | O G A (pixels equal the fixture) | O G A (`images_are_bounded_and_never_dropped`) |
| One suggestion per request; no regenerate or chat | O G A (reloads and a reply cause no call; unknown actions `422`) | O (provider usage count) | O (`a_non_exempt_ticket_gets_exactly_one_stored_suggestion`, `there_is_no_chat_or_regenerate_and_replies_do_not_decide`) | not applicable |
| Only the requester decides | O G A | O | provider-independent (`ai_decisions`) | not applicable |
| AI-solved read-only for its requester | O G A | O | provider-independent (`no_role_can_mutate_an_ai_solved_record`) | not applicable |
| AI-solved visible to Admin and in statistics | O G A (inspection, AI records, report numerator 3, usage per provider) | O | O | not applicable |
| AI-solved absent from every operator path | O G A (detail, messages, activity, four queue views with search, status filter, claim, dispatch, bulk, server-rendered page; Operator, Specialist, Team Lead) | O (queue, detail, search) | O G A (`w.in_queue`), O (`ai_stage_and_ai_solved_records_are_absent_from_every_operator_path`) | not applicable |
| AI-solved reopen denied, toggle off | O G A, seven actors (requester, Operator, Specialist, Team Lead, two Admins, another requester) | none | provider-independent (`no_role_can_mutate_an_ai_solved_record`) | not applicable |
| AI-solved reopen denied, toggle on | O G A, same seven actors; no reopen button | O (requester only) | provider-independent (`reopening_follows_the_admin_toggle_and_never_touches_ai_records`) | not applicable |
| Not-solved handoff exactly once | O G A (one event, one queue entry, repeat `200` without a second event, opposite decision `409`) | O | O G A | not applicable |
| Failed AI, provider `401`, handoff exactly once | O G A | none (`workflows.sh` on Compose: O) | O (`every_failure_hands_the_ticket_to_operators_exactly_once`) | O G A (`invalid_credentials_never_expose_the_key`) |
| Failed AI, malformed output, handoff exactly once | O G A | none | O G A | O G A |
| Failed AI, image refused by the provider | none | none | O G A (`vision_failed`) | none |
| Failed AI, timeout, `429`, `500`, outage | O only, in the recovery drill (hung call through SIGKILL, `503`, network disconnect) | none | O | O G A |
| German requester: German instruction and suggestion language | O G A | O | O (`the_snapshot_and_its_language_are_frozen_at_submission`) | not applicable |
| Statistics and records survive container recreation | O G A (usage, report, outcomes and records byte-identical) | not applicable | not applicable | not applicable |

Excerpt of the Compose stub log for one provider (metadata only):

```text
{"host":"api.anthropic.com","path":"/v1/messages","kind":"suggest","marker":"VALAI-TEXT","authenticated":true,"image_parts":0,"tools":false,"mentions_portal_url":false,"german_instruction":false}
{"host":"api.anthropic.com","path":"/v1/messages","kind":"suggest","marker":"VALAI-IMAGE","authenticated":true,"image_parts":1,"images_are_png":true,"tools":false,"mentions_portal_url":false,"german_instruction":true}
```

Totals on that stack after the three blocks: 23 submissions, 12 provider calls
(4 per provider: text, image, rejected credential, malformed output), usage
`{"requests":12,"succeeded":6,"failed":6}`, AI outcomes
`solved 3, not_solved 3, failed 6, skipped 6, category_exempt 3`. The stored
keys do not appear in a full `pg_dump`, and neither keys nor ticket text appear
in the API or web logs. The API container had only internal networks; an
outbound request to a public host from inside it failed.

Not covered on Compose for any provider: an image refused by the provider.
Not covered on Compose for Gemini and Anthropic: timeout, rate limit and outage
responses. Both run in process only.

## Clean production-style deployment

`docs/guides/install.md` was followed in order on project `tikkit-m4-52` with
locally built images, the private-bootstrap variant (`PUBLIC_HOST=localhost:18944`,
`PROXY_BIND=127.0.0.1`, `HTTP_PORT=18980`, `HTTPS_PORT=18944`,
`CADDY_GLOBAL_OPTIONS=local_certs`) and the unchanged reference Compose file.

| Step | Result |
| --- | --- |
| Step 2, environment file and secrets | The commands work as written (also the `sed -i.bak` form on macOS). File mode `0600`, git-ignored. |
| Step 4, `up -d --wait postgres`, `run --rm --no-deps api migrate`, `up -d --wait` | All exit 0. "Database migrations complete". Four services healthy. Rerunning `api migrate` later is a no-op. |
| Probes | `/api/health` `{"status":"ok"}`; `/api/ready` `{"status":"ready","checks":{"database":"ok","migrations":"ok"},"restore_mode":false}`. |
| Published ports | Only the proxy: `127.0.0.1:18944`, `127.0.0.1:18980`. API, web and PostgreSQL publish nothing. |
| Step 5, first Admin | Created in the browser; see criterion 9. |
| Step 6, organization | Two categories, two priorities, a conditional field, two teams with one lead each, ten accounts in all five roles, all sharing one contact email and staying ten accounts. |
| Step 8, backups | Key pair created, sidecar started, first backup complete 3 s later (231,076 bytes, 4 screenshot files), encrypted copy taken off the stack; no ticket text or username in the copy. |
| Restore section | See [backup and isolated restore](#backup-and-isolated-restore). |

Workflows on that stack, with a real browser (Chromium through Caddy) for
setup, invitation acceptance in English and German, submission with a
screenshot in English and German, dispatch, claim, start, proposal, reject,
accept, reopen, the Admin reopen toggle and cancellation, and the API with the
browser's headers (`Origin`, `X-Requested-With`, `Sec-Fetch-Site`) for the rest:

| Area | Result (status codes are the product's answers) |
| --- | --- |
| Team isolation after dispatch | The dispatcher from another team, another Team Lead and a staff member without a team get `404` on detail, messages, activity, screenshot download and preview, note, reply and claim; search in all four queue views returns nothing; the server-rendered page has no ticket content. Members, their lead and the Admin read `200`. |
| Claimed ticket | A colleague who is not the assignee reads `200` and gets `403` on note and reply. Assignee, Team Lead and Admin write `201`. |
| Notes | The requester sees 2 public messages where staff see 6; no note text in the messages API, the activity API or the page source. A requester note attempt is `403`. A replayed reply with the same idempotency key is stored once. |
| Self-handling | The submitting Operator gets `403` on dispatch, claim, note, start, proposal and priority, also after the ticket reaches their own team; Team Lead and Admin cannot assign it to them (`422`). Another account is assigned; the submitter's reply is stored as a requester reply. Team Lead and Admin are denied on their own tickets; a second Admin dispatches the first Admin's ticket. |
| Cancellation | Blank reason `422`, another requester `404`, staff `403`; the owner cancels in the German interface; the ticket leaves the queue and can be neither dispatched (`404`) nor reopened (`409`). |
| Audit and events | 144 audit events at that point, none without an account or a named service. `UPDATE` and `DELETE` on `audit_events` and `UPDATE` on `ticket_events` as the application role fail with "append-only". |

Two limits were met during the run and behaved as documented: invitation
acceptance answered `429 rate_limited` with `Retry-After` at the eleventh request
in one minute from one client, and ticket submission answered the same once one
account went past its documented ten per minute.

### Persistence across restart and recreation

A snapshot was taken through the API (10 accounts, teams, settings, catalog, 11
tickets with detail and messages, the requester's view and list, the SHA-256 of
a downloaded screenshot, the Admin report, AI usage, outcomes and records,
submissions) and from the database (hash over all audit events and all ticket
events). It was compared after each of:

| Operation | Result |
| --- | --- |
| `compose restart` (same containers) | 17 of 17 comparisons identical |
| `compose up -d --force-recreate --wait` (new containers) | 17 of 17 identical |
| `compose down` (volumes kept), then the install order again | 17 of 17 identical; same local CA, so `caddy-data` persisted; 4 screenshot files before and after |

AI statistics on this stack were zero because AI was off. Non-zero AI usage,
report, outcomes and records were compared on the AI stack across recreation of
the API and web containers and a PostgreSQL restart: all identical.

### SSR, hydration and navigation

| Check | Result |
| --- | --- |
| Two users, page sources compared | Each server-rendered history holds the user's own tickets; no subject, username or note of the other user. The requester's own page names the assigned staff member, which is intended. |
| 30 interleaved concurrent renders for two users | No mixed content. |
| Headers | `cache-control: private, no-store`, `vary: Cookie, Accept-Language`, `x-sveltekit-page: true`. |
| Deep links | Another user's ticket URL: `404` page without content. Signed out: redirect to `/login?next=…`, then the requested ticket after sign-in. An external `next=` is not followed. A staff ticket URL renders with content on the server. |
| Client navigation | List to detail and back without a document request; no console or page error. |
| Route guards | A requester gets `403` on `/queue` and `/admin/settings`, an Operator on `/admin/audit`. |

### Reverse proxy through the running stack

`ops/caddy/Caddyfile` as integrated with the security branch, each rule tested
through the published port:

| Rule | Test | Result |
| --- | --- | --- |
| Only the configured host is served | `Host: evil.example:18944`, `Host: localhost:443`, `Host: localhost` over HTTPS; a foreign host over HTTP; TLS to the IP address or a foreign SNI | `421` for each host header; `421` over HTTP; TLS handshake refused for the IP and the foreign name. |
| HTTP redirects to HTTPS | `http://localhost:18980/setup?x=1` | `308` to `https://localhost:18944/setup?x=1`. |
| Forwarding headers overwritten | Failed sign-in with `X-Forwarded-For`, `X-Real-IP` and `Forwarded` set to `203.0.113.77` | The audit event records the real peer; no event contains the spoofed address, and none carries an address of the proxy network. |
| `X-Forwarded-Proto` and `X-Forwarded-Host` | Request with `http` and `evil.example` | `200`, HSTS still sent, no reference to the spoofed host. |
| `X-Request-ID` stripped | Request with a valid UUIDv4 as ID to `/api/health` | The response carries a different ID; the forged ID is in no API or web log line. |
| `nosniff` and `frame-ancestors` added without clobbering | Page, API error, static asset, `421`, SvelteKit's own `403` | A page has exactly one CSP header, the application's nonce policy; an API error has exactly one, the API's `default-src 'none'`; the asset, the `421` and the plain `403` carry the proxy's two headers; HSTS everywhere. |
| Screenshot responses | Authenticated download | `content-type: image/png`, `content-disposition: attachment`, CSP with `sandbox`, `nosniff`. Unauthenticated: `401`. |
| Body limit | 11 MB upload with and without `Content-Length` | `413` from the proxy in both cases. A 2 MB JSON body below the proxy cap is refused by the API with `413 payload_too_large`. |
| Timeouts | Unfinished request header; unfinished request body, raw TLS socket | Connection closed after 10.0 s; `499` after 45.0 s. |
| Metrics | `/metrics`, `/metrics/`, `/metrics/x`, `/api/metrics`, and without a metrics port also six encoded, upper-case or dotted variants | `404` for all. With `METRICS_PORT=9091` the listener is `127.0.0.1:9091` inside the API container: not published on the host, connection refused from the web and proxy containers. 100 samples, none with ticket text or usernames. |

Origin policy through the proxy, on `POST /api/auth/login`:

| Request | Status |
| --- | --- |
| Custom header, `Origin` equal to the public origin, `Sec-Fetch-Site: same-origin` | `200` |
| Custom header, no browser metadata (a script) | `200` |
| No `X-Requested-With` | `403` |
| `Origin: https://evil.example`, `http://localhost:18944`, `https://127.0.0.1:18944`, `https://localhost`, `null` | `403 csrf_rejected` each |
| `Sec-Fetch-Site: cross-site` or `same-site` | `403` |
| CORS preflight | `405`, no `access-control-allow-*` header |
| Cross-site form `POST` to a page | `403` |

Session cookie: `HttpOnly; SameSite=Lax; Path=/; Max-Age=1209600; Secure`,
without a `Domain`.

## Operational readiness

### Backup and isolated restore

`ops/backup/drill.sh` in full mode with projects `tikkit-m4-52-drill` and
`tikkit-m4-52-drill-restore` (opt-in flag) and port base 18950: **101 checks
passed**, exit 0.

| Timing | Value |
| --- | --- |
| Source stack start | 23 s |
| Seeding | 3 s + 9 s |
| Backup run | 1 s, 207,362 bytes |
| Restore total | 26 s (database 1 s, screenshots 0 s) |
| Whole drill | 236 s |

The drill's checks include: project-name refusals, encrypted and private backup
files, off-host copy and its verification without the key, rotation,
simultaneous runs, a failed backup when the database session is lost, the
retention routine deferring a file deletion while the backup holds the lock, a
restore that refuses an occupied project, equality of ticket content, statistics,
audit counts and screenshot bytes between source and copy, the email cutover,
AI-solved rules on the copy, restore-mode refusals, a missing backup key, a
missing and a wrong `ENCRYPTION_KEY`, and a backup with a missing screenshot
file. The dataset is small; the timings say nothing about a large installation.

Independently of the drill, the backup taken from the install-guide stack was
restored with the guide's own restore section into `tikkit-m4-52-restore`:
`restore.sh` exit 0 in 27 s, "rows without a file: 0; files without a row: 0".
Compared with the source at backup time: accounts, teams, settings, all 11
tickets with messages, the requester's view, the screenshot hash, the Admin
report, all ticket events and all 186 audit events are identical.

### Restore mode, retention, audit and rate limits

On the restored copy:

| Interaction | Result |
| --- | --- |
| Readiness and banner | `"restore_mode":true`; the Admin sees the banner, a requester does not. |
| Retention run | `409 restore_mode_blocked`, no run row. |
| Anonymization of a deactivated account | `409 restore_mode_blocked`; the account keeps its username. |
| Automatic retention setting | Saving "apply automatically" with the typed confirmation is accepted (`200`). No worker runs in restore mode, so nothing is deleted there; it takes effect when the copy leaves restore mode. See F-3. |
| Outbound | Password recovery `409 outbound_disabled` for a known and an unknown user alike, no reset token. AI and SMTP connection tests `409 outbound_disabled`. An invitation with an email address is created with `emailed: false`. |
| Audit | The email cutover is one event, `notifications_suppressed_on_restore` by service `restore`, with a cutoff and counts only. Portal actions on the copy are still authorized and audited (a reply `201`, a reopen `403`, audit count 197 to 200). |
| Rate limits | Five failed sign-ins for one username `401`, then `429`. |
| Workers | A proposal backdated past its deadline is still `awaiting_confirmation` after 20 s. |

On the install-guide stack: a non-Admin gets `403` on retention preview, read and
run. A run with the wrong confirmation word is `422`, with a forged token or a
shifted clock `409 retention_preview_stale`. A confirmed run with nothing due
completes, deletes nothing and leaves `retention_settings_updated`,
`retention_run_started` (the Admin) and `retention_run_completed` (service
`retention`) in the audit log. A burst of previews in one minute was limited
with `429`. See F-4 for the preview token.

### Load and performance

`ops/load/run.sh smoke` (two lanes, 12 requesters, 12 historical tickets, five
seconds, private SMTP relay on): result `pass`. Reconciliation: 2 submissions,
2 submit keys, 2 submit events, 2 confirmations, 2 replies, 2 notes, 1
attachment, 2 claims, 0 duplicate notifications, 10 emails sent, outbox empty.
Races exercised: 2 submit, 2 claim, 2 confirmation. Client JavaScript: 60
files, 172,604 bytes gzip, budget 204,800.

`ops/load/run.sh profile` (30 lanes, 300 requester accounts, 30,000 historical
tickets, 300 s after a 12 s warm-up, container limits of 4 vCPU and 4 GiB in
total, private SMTP relay and HTTPS provider stub): exit 0, result `pass`,
every lane completed all scheduled cycles.

| Endpoint group | Samples | Target p50 / p95 / p99 (ms) | Measured p50 / p95 / p99 (ms) | Result |
| --- | ---: | ---: | ---: | --- |
| Sign-in | 1,530 | 150 / 450 / 900 | 13.4 / 17.7 / 20.4 | Met |
| Ticket submission | 3,000 | 100 / 300 / 750 | 5.1 / 6.3 / 8.5 | Met |
| Screenshot upload | 300 | 100 / 300 / 750 | 6.2 / 8.1 / 8.6 | Met |
| Requester reads | 4,124 | 75 / 200 / 500 | 1.4 / 2.0 / 2.9 | Met |
| Queue, search and staff detail | 9,657 | 100 / 300 / 750 | 1.9 / 40.8 / 49.0 | Met |
| Workflow writes | 3,630 | 100 / 300 / 750 | 1.9 / 4.6 / 5.9 | Met |
| Admin report | 100 | 750 / 1,000 / 1,500 | 763.1 / 910.8 / 951.4 | **p50 missed by 13.1 ms**; p95 and p99 met |
| Admin submissions | 100 | 100 / 300 / 750 | 20.1 / 21.8 / 24.5 | Met |
| Admin audit | 100 | 100 / 300 / 750 | 1.6 / 1.9 / 2.3 | Met |
| Admin combined comparison | 300 | 250 / 750 / 1,500 | 20.1 / 884.3 / 923.9 | **p95 missed**; p50 and p99 met |

Targets are those of `docs/performance-baseline.md`. The two misses are the
same two that page already records for the capped reference (report p50 781.6
ms there, 763.1 ms here): the Admin report over 30,000 tickets under a 2-vCPU
database cap. They are recorded as finding F-6, not rounded up.

Reconciliation of the same run: 1,501 submissions with 1,501 submit keys and
1,501 submit events; 251 AI attempts, 126 solved and 125 not solved; 313
confirmed proposals, 313 public replies, 313 notes, 301 attachments, 313
claims; 0 duplicate notifications; 0 staff notifications for AI-solved
records; 3,926 ticket emails recorded as sent (the relay accepted 3,927; the
smoke run shows the same difference of one, 10 and 11); outbox empty at the
end. Races exercised: 1,501 submit, 313 claim, 313 confirmation, 251 AI
decision; 191 operator access passes against AI-solved records.

Sampled peaks: API 42% of one CPU and 98 MiB of 768 MiB; PostgreSQL 174% CPU
and 407 MiB of 2 GiB; 4 active of 10 pooled API connections, 10 of 100 client
backends. Worker backlog peaks: notifications 1,800 pending with the oldest
due 2.0 s (target 60 s); auto-close 167 pending, none overdue; AI and
retention none overdue.

The browser timing run (`ops/load/run.sh frontend`) and the 10,000-row export
diagnostic were not repeated for this record.

### Recovery scenarios

`ops/load/recovery.sh` without `--long` on a fresh seeded stack with container
limits: exit 0, **11 of 11 scenarios passed**, 0 duplicate emails among 147
accepted, 0 duplicate outbox rows.

| Scenario | Result |
| --- | --- |
| `sigkill_ai_in_flight` (API killed during AI attempts) | Passed in 154.2 s; the usage lease was released after 154.2 s. |
| `postgres_restart_in_flight` | Passed in 6.3 s. The interrupted request answered `500`, the submission receipt was preserved, two reads were unavailable during the restart. |
| `overdue_proposals_two_workers` (two API replicas) | Passed in 21.3 s. 10 proposals timed out, 4 were requester-confirmed, closures by replica `[1, 9]`, maximum lag after the deadline 1,975 ms, no ticket closed twice. |
| `api_restart_mid_load` | Passed: 1 committed, 7 clean failures. |
| `provider_network_disconnect` | Passed in 25.7 s; queue reads p99 91 ms during the fault. |
| `provider_slowness_and_outage` | Passed in 29.1 s; at most 2 provider calls at once, 8 refusals while the circuit was open. |
| `sigkill_unacknowledged_smtp`, `sigkill_after_smtp_receipt` | Passed. |
| `smtp_stop_mid_delivery`, `mail_outage_retry` | Passed; `accepted_after_ms` 31,914 for the retried mail. |
| `retention_catches_up_after_restart` | Passed; processing lag 5.7 s. |

The `--long` variant (a provider call hung through the full 120-second
deadline) was not run for this record.

### Confirmation auto-close in Compose

The reference Compose file has four services and no scheduler, queue or cron
service. The API process starts the workers itself when it is not in restore
mode; the private metrics show `tikkit_worker_last_success_timestamp_seconds`
for `ai_attempts`, `ai_attempt_sweep`, `notifications`, `auto_close` and
`retention`. On the install-guide stack a proposal whose
`confirmation_deadline` was moved one minute into the past with SQL (the
approach of the restore and recovery drills; the product has no test seam for
it) was closed by the worker within 2 s with `closure_reason: confirmation_timeout`.

### Release workflow (static)

Read, not run: no release was created and nothing was published by this
validation, and none exists (`gh release list` and `git ls-remote --tags` are
empty).

| Part | Finding |
| --- | --- |
| `release-please-config.json`, `.release-please-manifest.json` | Manifest mode, components `web` (`apps/web`, node) and `api` (`apps/api`, rust) at 0.1.0, tags `web-vX.Y.Z` and `api-vX.Y.Z`, `cargo-workspace` plugin. |
| `.github/workflows/release-please.yml` | Runs on pushes to `main` in environment `release` with a GitHub App token, then calls the image workflow for each tag it created. Top-level `permissions: {}`. |
| `.github/workflows/release-images.yml` | Reusable and manually dispatchable. Refuses anything but `main` and a matching `web-v`/`api-v` tag, resolves the tag to a commit that must be an ancestor of `origin/main`, runs `ci.yml` on that exact revision, then publishes in environment `release`, refusing an existing registry tag and aborting when absence cannot be confirmed. `linux/amd64`, provenance `mode=max`, OCI source, revision and version labels. |
| `.github/workflows/ci.yml` | Accepts `workflow_call` with `component` and `revision`. |
| Pins | Actions are pinned to commit SHAs; `actionlint` reports nothing. |

### Scope and dependencies

| Question | Finding |
| --- | --- |
| SLA configuration, deadline scheduler, breach dashboard, reminders | No match for SLA or breach terms in `apps/api/src`, `apps/api/migrations`, `apps/web/src`, the Compose files, the env examples or `ops/`. |
| Knowledge-base service or retrieval | None. The only "article" match in the API is the prompt rule "Do not invent sources, links, article names, or reference numbers". |
| Inbound email | No IMAP, POP3, webhook or mail-parsing code or dependency. `there_is_no_inbound_email_or_webhook_channel` passes. |
| SSO | No OIDC, OAuth, SAML or LDAP code or dependency. |
| Hosted platform, object storage, extra services | Compose services are `postgres`, `api`, `web`, `proxy`; optional overlays add the backup sidecar and Prometheus. Screenshots live in a named volume. |
| Dependencies of #52 | #55 and #4 are closed. #44, #48, #50 and #51 are implemented by this stack and close when it merges. #29 and #30 (SLA) are open and unreferenced. |

### Accessibility

The axe-core scan (`e2e/a11y.spec.ts`: WCAG 2.2 A and AA, every route, every
role that can open it, English and German, three widths, zero violations
allowed), the keyboard spec and the failure-recovery spec are part of the 156
passing stub tests. No person walked the screen-reader checklist in
`docs/accessibility.md`.

## Cross-branch review

Things no single-branch review covered.

| Item | What was done | Finding |
| --- | --- | --- |
| Proxy configuration after the security integration | Each rule tested through the running proxy | As specified; see [the table above](#reverse-proxy-through-the-running-stack). |
| Restore mode with retention, audit and rate limits | Tested on a restored copy of a worked stack | Consistent; F-3 and F-4 are observations. |
| #127, last commits (`fb17a18`, `4f7740a`, `b99dfe1`) | Diffs read; unit tests and the stub browser suite run at the candidate | No defect. `mergeCatalog` makes German fall back to English for a missing string; the catalogs were compared (1,098 strings, 0 missing, 0 blank, 36 identical values that are names or cognates) and `English and German expose the same message keys` guards it. The dialog no longer announces its own opening, which a native modal does itself. |
| #128, later commits (`6373375`, `bb592b0`, `147c4a0`, `6e5df54`, `b950fd1`) | Diffs read; tests, smoke, profile and recovery run | No defect. `SMTP_CA_FILE` adds trust anchors for SMTP only, is validated at startup without printing the path, and never disables verification. The report volume queries now read tickets directly with the same scope filter and `status <> 'draft'` as the detail cohort; `SET LOCAL jit = off` is limited to the report transactions. |
| Authorization order | Seen during the run | A malformed body is answered `422` before the ticket lookup. A foreign ticket and an unknown ID get the same answer, so it reveals nothing about existence. |

## Documentation against the product

Eleven claims from `docs/guides` checked on the running stack, all correct:

| Claim | Check | Result |
| --- | --- | --- |
| Subject up to 200, description up to 20,000 characters (`requester.md`) | 200/201 and 20,000/20,001 | `201`/`422` and `201`/`422` |
| PNG, JPEG or WebP, decided by content (`requester.md`) | PNG, JPEG, SVG named `.png`, GIF, HTML, PNG sent as JPEG | `201`, `201`, then `unsupported_type` three times and `type_mismatch`. WebP was not tried. |
| At most 10 screenshots per ticket (`requester.md`) | 11 and 10 | `422`, `201` |
| Cancellation reason required, up to 1,000 characters (`requester.md`) | 1,001 and 1,000 | `422`, `200` |
| The dispatcher outside the team sees "Ticket #N was sent to … You are not a member of that team, so you can no longer open it." (`operator.md`) | Browser | Exact text shown |
| Resolution summary required, up to 5,000; replies up to 10,000 (`operator.md`) | Blank, 5,001, 5,000; 10,001, 10,000 | `422`, `422`, `200`; `422`, `201` |
| Bulk actions take up to 50 tickets, each checked on its own (`operator.md`) | 51 and 50 unknown IDs | `422`; `200` with 50 `not_found` results |
| A password needs at least 12 characters (`requester.md`) | 11 characters | `422` |
| API logs are JSON and `x-request-id` finds the log line (`install.md`) | Request ID of a response searched in `compose logs api` | Found; the line has the route template, no query string and no content |
| Quoted portal messages (`troubleshooting.md`) | Four messages searched in the English catalog | All present verbatim |
| The quoted `csrf_rejected` messages (`troubleshooting.md`) | API response and catalog | Both verbatim |

Three symptoms from `docs/guides/troubleshooting.md` reproduced:

| Symptom | Reproduction | Result |
| --- | --- | --- |
| The API refuses to start with `TRUST_PROXY_HEADERS` | `compose run --rm --no-deps -e TRUST_PROXY_HEADERS=true api` | Exit 1 with "invalid configuration" and "TRUST_PROXY_HEADERS was replaced by TRUSTED_PROXIES…", as documented. A short `ENCRYPTION_KEY` lists both problems and prints no value. |
| Readiness reports `migrations: incompatible` after a rollback | A migration row unknown to the image inserted into the disposable database | `503` with `"migrations":"incompatible"`; `/api/health` stays `ok`; `api migrate` exits 1. The documented JSON lacked the `restore_mode` field (F-1, corrected). |
| Too many requests (`429`) | Invitation acceptance and ticket submission past their per-minute limits | `429 rate_limited` with `Retry-After`. |

Also as documented: `compose run --rm --no-deps api credentials-check` prints
`ai_credentials=none smtp_credentials=none`; `/setup` after setup leads to the
sign-in page; a used invitation link shows no account form.

## Findings

No finding is a functional defect and none blocks the release. No product
behaviour was changed to make a check pass.

| ID | Severity | Finding | Action |
| --- | --- | --- | --- |
| F-1 | Low, documentation | `docs/guides/troubleshooting.md` showed the readiness body without the `restore_mode` field the API returns. | Corrected on this branch (`docs: show the restore_mode field in the readiness example`). |
| F-2 | Low, hygiene | `bun audit --audit-level=high` reports nothing without `--ignore=GHSA-fc5q-39xc-vjpf`, so the documented picomatch exception appears to be no longer needed. | Reported. Owner: F1nal04. Closed by removing the `--ignore` from `.github/workflows/security.yml` and the entry from `docs/security-scanning.md` after confirming on CI. |
| F-3 | Low, by design | A restored copy in restore mode accepts turning on automatic retention. Nothing is deleted while restore mode is on; deletion starts within a day of leaving it. `docs/backup-restore.md` describes the setting as part of the restored database. | Reported. Owner: F1nal04. Closed by a sentence in the post-restore checklist, or by refusing the automatic setting in restore mode like a run. |
| F-4 | Low | A retention preview token can start a second run after the first completed, while it is still fresh (`202`). The second run uses the same clock and policy, so it removes only what the reviewed preview covered. | Reported. Owner: F1nal04. Closed by making a token single-use, or by documenting the behaviour. |
| F-6 | Low, performance | Under the capped reference load the Admin report misses its p50 target by 13.1 ms (763.1 against 750) and the combined Admin comparison misses its p95 target (884.3 against 750). Every other group is far inside its target; `docs/performance-baseline.md` already records both misses. Only the Admin report page is affected. | Reported. Owner: F1nal04. Closed as described in the risk table. |
| F-5 | Medium, repository settings | The `release` environment, the tag rulesets and the required `compose` check do not exist yet (read through the GitHub API). `release-please.yml` runs on every push to `main`. | See [repository settings](#repository-settings-the-owner-must-apply). |

Fifteen scripted checks failed when first evaluated. Fourteen were errors of
the validation driver and passed when repeated with correct input: request
bodies without the required `idempotency_key`, requests not paced under the
documented rate limits, a wrong column name in two database probes, a string
comparison of numbers, a timing measurement that measured its own `sleep`, and
the expectation that a requester's page must not name the assigned staff
member. The fifteenth is F-3.

## Known limitations and remaining risks

Owner of every item: the repository owner, **F1nal04**.

| Risk or limitation | What would close it |
| --- | --- |
| No release exists: no tag, no registry image, no release commit on `main`. Criterion 1 is open. | Merge the stack, let CI run on the `main` commit, publish the first release through the release workflow, record the image digests. |
| AI was validated against stubs only. A real provider may differ in response details, limits and model behaviour, including whether the model follows the prompt-safety instructions. | One submission per provider with a real key on a test installation: text-only and with a screenshot, plus the Admin connection test. |
| No real SMTP relay was used. Delivery ran against capture relays with STARTTLS and implicit TLS. | Configure a real relay on a test installation; send the test mail, an invitation and a recovery mail in English and German. |
| Public DNS and ACME were not exercised; every stack used Caddy's local CA on loopback. | First installation on a host with a public name: certificate issuance and renewal. |
| No dedicated Linux x86-64 host. All runtime evidence is from Docker Desktop on ARM64. | Run the install guide and the restore drill once on the target host class. |
| Performance figures are from one workstation with container limits. Two latency targets are missed there (F-6): Admin report p50 763.1 ms against 750, and the combined Admin p95 884.3 ms against 750, on 30,000 tickets. | Measure on the target host class; either tune the report aggregates or set the report target from the measured cost, and record the decision in `docs/performance-baseline.md`. |
| A human screen-reader pass is outstanding. | A person walks the checklist in `docs/accessibility.md` with VoiceOver or NVDA. |
| One API replica is supported: rate limits and budgets are per process and reset on restart. | Accepted for this release; documented. |
| Email delivery is at least once across a crash. | Accepted; documented. |
| Concurrent-write rules for teams and roles are proven in process, not on Compose. | Accepted; the same code runs in both. |
| On Compose, an image refused by the provider was not exercised for any provider, and timeout, rate limit and outage only for OpenAI. | Accepted; covered in process for all three. |
| WebP upload and the 120-second hung-provider case (`recovery.sh --long`) were not run for this record. | Covered by the integration suite (`each_supported_format_is_decoded_reencoded_and_served_safely`) and by the earlier recorded long run in `docs/performance-baseline.md`; rerun `--long` before a release if the AI worker changes. |
| Two audit exceptions (`RUSTSEC-2023-0071`, `RUSTSEC-2026-0459`) have no tracking issue; review date 2027-02-18. | File the issues the exception policy asks for, or remove the exceptions when sqlx drops the crates. |
| F-2, F-3, F-4 | As listed under [findings](#findings). |

## Not verifiable on this machine

- **Real provider keys.** No call to OpenAI, Google or Anthropic was made, by
  design of the test rules.
- **A real SMTP relay.**
- **Public DNS and ACME** certificate issuance.
- **GHCR publishing and release tags**: Release Please, the tag rules, the
  `release` environment gate and the first authenticated push need the
  repository settings and a run on `main`.
- **CI on the release commit** (criterion 1).
- **A dedicated Linux x86-64 host**, and therefore the published image
  architecture outside CI.
- **A human screen-reader pass.**
- **Whether a real model understands a screenshot** or obeys the prompt rules.

## Repository settings the owner must apply

State read through the GitHub API on 2026-11-19: `main` requires `api`,
`api-release` and `web`; there is no environment, no ruleset; secret scanning
and push protection are on. The exact steps are in
[ci.md](ci.md#repository-owner-setup).

Before merging #124 (it adds `release-please.yml`, which runs on every push to
`main`):

1. Create the **`release` environment** with deployment branches restricted to
   `main` and no tag rule; add required reviewers if releases should need
   approval. If the workflow runs first, GitHub creates the environment without
   any protection and the run fails at the token step.
2. Create and install the **Release Please GitHub App** on this repository
   (Contents, Pull requests and Issues: read and write) and store
   `RELEASE_APP_CLIENT_ID` (variable) and `RELEASE_APP_PRIVATE_KEY` (secret) in
   the `release` environment.
3. Create the two **tag rulesets** for `web-v*` and `api-v*`: one restricting
   creation with the App as the only bypass actor, one restricting updates and
   deletions with no bypass.

After #124 is on `main` and GitHub has recorded a `compose` run:

4. Add **`compose`** to the required status checks of `main`, keeping `api`,
   `api-release` and `web`. Do not make `audit` required.
5. In the two GHCR package settings, grant this repository Actions write access
   if needed, remove other writers, and decide on public visibility.

## Release decision

**Go, with these conditions:**

1. Merge the 12 pull requests bottom-up and confirm the required checks are
   green on the resulting `main` commit. That closes the CI half of
   criterion 1.
2. Apply the repository settings above, items 1 to 3 before #124 merges.
3. Publish the first release through the release workflow and record the two
   image digests with the release commit. That closes the artifact half of
   criterion 1. Until then installations build from source as the guide says.
4. Before announcing AI assistance or email to users, run the real-provider and
   real-relay checks from the risk table on a test installation.
5. Treat the remaining rows of the risk table as accepted for the first
   release, each with its owner.

Nothing found in this validation requires a code change before the release.
F-6 is a decision for the owner: accept the measured Admin report latency for
the first release, or tune it first.
