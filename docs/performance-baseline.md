# Single-host capacity and performance

The reference workload represents a single help desk with independent
requester and staff sessions, manual team dispatch and resolution, and local
AI and SMTP peers. Build and run the pinned production images through Caddy
using the [runner](../ops/load/README.md). The full workload, recovery drill,
and browser timing are manual. The existing Compose CI job runs a five-second
workflow smoke and a client JavaScript size check.

## Single-host release targets

Reference profile for #50, fixed before measurement: 30 concurrent virtual
users for 120 seconds, 300 requester accounts, three teams, 30,000 historical
submissions, and a 250 ms deterministic HTTPS provider. Time requests from the
load driver through Caddy, including body transfer. Report percentile sample
counts with nearest-rank p50/p95/p99. Expected replay and conflict responses are
successful assertions. Report unexpected HTTP errors separately from them.

| Endpoint group | p50 / p95 / p99 target (ms) |
| --- | ---: |
| Sign-in | 150 / 450 / 900 |
| Submit, upload | 100 / 300 / 750 |
| Requester history and detail | 75 / 200 / 500 |
| Queue, search, staff detail | 100 / 300 / 750 |
| Replies, notes, assignment, resolution | 100 / 300 / 750 |
| Admin reports, submissions, audit | 250 / 750 / 1,500 |

- Unexpected requests under 0.5%, zero transport failures in the steady
  window, zero invariant failures, and no pressure-induced 429 responses.
- Peak active DB connections below the 10-connection API pool cap and total
  client backends below 50% of PostgreSQL's `max_connections`.
- Maximum sampled due lag below 15 s for AI attempt sweeps, 60 s for outbox,
  5 s for auto-close and 30 s for retention. A zero must distinguish an idle
  worker from one that caught up. Successful AI attempts complete within 10 s.
- Each container below 200% sampled CPU (two cores) and 1 GiB resident memory.
- On unthrottled loopback, queue TTFB/FCP/LCP below 500/1,000/1,500 ms,
  click-to-next-paint below 200 ms, and queue-to-detail heading below 750 ms.
  Cold queue initial JS transfer at most 200 KiB, all client JS compressed
  at most 512 KiB. Track these budgets separately from API/provider timings.

## Reference environment

Measured 2026-11-19 on an Apple M4 Pro developer workstation running macOS
27.0.1 (26A434), with 14 cores (10 performance, 4 efficiency) and 48 GiB RAM.
The arm64 Docker Desktop Linux VM reported 14 vCPUs and 7.75 GiB RAM
(Engine 29.8.2, Compose 5.6.0). Production Compose sets no individual CPU or
memory cap. All six containers share the VM's capacity. The client and browser
ran on the host against Caddy over loopback HTTPS, with no CPU or network
throttling. The browser measurements ran in a separate locked window.
These observations describe one workstation and one workload.

The profile uses 300 requester accounts, 61 operator accounts, three team
leads, one Admin, and 30,000 historical tickets. Requesters rotate across
lanes with at least six seconds between cycles. Staff accounts opt out of test
notifications so the relay sees requester volume while outbox authorization
and suppression still run. The [runner](../ops/load/README.md) documents the
mix of ticket states, roles, payloads, and commands.

## Reference API and worker results

Nearest-rank p50 / p95 / p99, milliseconds through Caddy. Expected conflicts and
denials count toward sample totals. The Admin group combines reports,
submissions and audit requests.

| Group | Samples | Target p50 / p95 / p99 (ms) | Measured p50 / p95 / p99 (ms) | Target |
| --- | ---: | ---: | ---: | --- |
| Sign-in | 630 | 150 / 450 / 900 | 13.8 / 20.1 / 23.1 | Met |
| Submission and upload | 1,320 | 100 / 300 / 750 | 5.0 / 6.7 / 10.0 | Met |
| Requester reads | 1,617 | 75 / 200 / 500 | 1.4 / 2.0 / 3.9 | Met |
| Queue, search and staff detail | 3,959 | 100 / 300 / 750 | 2.0 / 43.1 / 48.7 | Met |
| Workflow writes | 1,450 | 100 / 300 / 750 | 2.0 / 4.7 / 8.3 | Met |
| Admin reads | 60 | 250 / 750 / 1,500 | 20.7 / 680.4 / 708.5 | Met |

- 0 unexpected requests out of 9,036. The profile produced 601
  unique submissions including the preflight sentinel. 121 included
  screenshots. There were 101 completed AI attempts, 51
  explicit AI-solved decisions, 50 escalations, and
  125 requester-confirmed human closures. Claims, reply/note
  counts and immutable events matched. The operator visibility checks found
  zero AI-solved leaks and zero staff outbox recipients for AI-solved tickets.
  Admin reports reconciled to 30,000 historical tickets plus the live submissions.
- 1,562 durable ticket mail receipts matched the capture count
  after excluding the setup probe. All outbox jobs drained and the duplicate
  checks found zero duplicate events or mail acceptances.
- PostgreSQL peak was 3 active or in-transaction
  API-role client backends, 8 API-role client connections
  open (pool cap 10, including a possible health probe), and
  8 total client connections out of
  `max_connections=100`. PostgreSQL parallel query workers
  are excluded. Samples include the time points the collector observed.
- The stub returned in 251.0 / 256.0 / 256.0
  ms p50/p95/p99. End-to-end AI attempt completion took
  1,560.9 / 1,823.3 / 1,876.2 ms p50/p95/p99,
  with maximum 2,311 ms. API timing above excludes
  provider waits. The gap includes the worker poll, admission, image
  preparation, network transfer, and persistence.

| Worker | Peak pending jobs | Peak overdue lag | Target lag |
| --- | ---: | ---: | ---: |
| AI attempt sweep | 2 | 0.00 s | 15 s |
| Notification outbox | 1,918 | 3.32 s | 60 s |
| Auto-close | 0 | 0.00 s | 5 s |
| Retention | 0 | 0.00 s | 30 s |

Auto-close and retention stayed idle in the steady window. Their catch-up
on real jobs is in the recovery results below. AI attempt sweep lag tracks
overdue sweeps, while the completion time above measures active jobs.

| Container | Sampled peak CPU | Sampled peak memory working set |
| --- | ---: | ---: |
| api | 42.84% | 69.0 MiB |
| postgres | 160.06% | 354.6 MiB |
| provider | 0.36% | 23.7 MiB |
| proxy | 3.43% | 36.8 MiB |
| smtp | 3.62% | 32.8 MiB |
| web | 9.36% | 43.7 MiB |

Docker stats sampled all containers 39 times
at roughly 3.24 s
intervals. 100% CPU means one core. The web container was lightly exercised
during the API window. Browser performance was measured separately.

### Tuning from the measurements

The untuned API image at `ea15113` missed the Admin group p95/p99 and database CPU targets,
at 1,620.3/1,644.7 ms
and 219.62% respectively.
An isolated report-only test with seven warmed samples per setting measured
p95 1,572.4 ms with PostgreSQL's default JIT and 906.6 ms with JIT disabled.
The two configurations returned identical reports. The report read now uses a
transaction-local JIT setting and computes daily, category, and priority
totals directly from the scoped tickets. The full rerun at `6efc746` measured Admin
p95/p99 680.4/708.5 ms and PostgreSQL peak CPU
160.06%.

The remaining query, upload and worker measurements supported keeping the
existing page sizes, 10-connection pool, upload caps, worker concurrency, and
retry policy. This issue added zero indexes and migrations.

To repeat the JIT diagnostic before the change, pass the pre-tuning API image
to `ops/load/run.sh report-diagnostic`. It changes the DB role's JIT setting
only in the disposable project and restarts the API to open fresh connections.

## Recovery on Compose

| Injected condition | Verified outcome |
| --- | --- |
| Graceful API restart during submissions | 1 wave commits and 7 clean transient failures. All eight keys converged to one durable ticket and submit event each. The committed outbox and AI attempt survived. |
| PostgreSQL stop/start | Liveness stayed at HTTP 200, readiness went to 503, then recovered to 200 without restarting the API. |
| SIGKILL during provider request | One queued handoff, zero AI resolutions. The disconnected result could not commit. The old usage lease cleared after 153.4 s. |
| Slow provider followed by HTTP 503 | Peak provider calls 2. The breaker refused 8 calls. Direct exempt submit took 17 ms. |
| SIGKILL while the relay held DATA | The restarted worker retried. The relay acknowledged one copy per recipient and the committed ticket survived. |
| SIGKILL after durable SMTP receipt | The retried outbox row reused the durable receipt. The relay acknowledged one copy per recipient. |
| SMTP 451 then recovery | Both recipients retried after the 30 s backoff, accepted after 32.4 s. No duplicate relay acceptance. |
| Overdue proposals, two API replicas and decisions | 8 timeout closes and 4 requester closes. Every proposal had one closure event and notification. Replies and notes persisted and report totals matched. |
| Retention restart catch-up | The pending no-op retention run finished after 5.68 s including simulated age. |

The dashboard histograms and counters are local to each process. The results
above reconcile against PostgreSQL and relay acceptance after restarts. The
two-replica exercise tested only the data layer. One API replica is supported
because rate-limit counters are process-local. The SMTP scenarios bracket the
ambiguous SMTP outcome where a relay accepts mail before a receipt can be
committed. Generic SMTP cannot guarantee one copy across that gap. See
[the operations runbook](operations.md#load-and-recovery-drill).

## Production web performance

Seven cold headless Chromium 156.0.8078.4 contexts at 1440 × 900 view
the production queue with the same 30,000-ticket fixture. Playwright observes
the Navigation, Paint, LCP, and Resource Timing entries, and measures one
selection click and both queue/detail navigation paths. Reported p95/p99 are
the maximum with seven samples. The click-to-paint proxy uses a second
animation frame. Field INP needs longer representative browser sessions.

| Metric | p50 / p95 / p99 (ms) | Budget |
| --- | ---: | ---: |
| Queue TTFB | 33.7 / 66.9 / 66.9 | 500 ms |
| Queue FCP | 88.0 / 144.0 / 144.0 | 1,000 ms |
| Queue LCP | 88.0 / 144.0 / 144.0 | 1,500 ms |
| Selection click to paint | 30.9 / 31.6 / 31.6 | 200 ms |
| Queue to detail heading | 35.0 / 43.5 / 43.5 | 750 ms |
| Detail back to queue heading | 46.9 / 54.4 / 54.4 | Tracked |

Cold queue JavaScript transferred **85,074 bytes**
against a 204,800-byte budget, including resource transfer overhead. The encoded
body bytes were 76,974. CI checks **all** browser
JavaScript from the production image at gzip level 9. It measured
169,409 bytes across 59 files, against 524,288
bytes. That gate is independent of this browser timing run.

## Evidence and limits

The [dated results directory](../ops/load/results/2026-11-19) contains aggregate
before/after profiles, the report JIT diagnostic, host and bundle facts,
browser samples, the short smoke, and recovery outcomes. Reproduce them via
[the runner](../ops/load/README.md). Readiness, worker metrics and recovery
actions are documented in the [runbook](operations.md#load-and-recovery-drill).

This workstation run provides no sustained-load, cold-disk recovery, public
internet, or external-provider throughput measurement. Resource peaks are
sampled. Automatic closure and retention were exercised in the fault drill
with a small number of due jobs. Large retention backlogs, very large mail fan-out, and higher concurrency tiers
remain unmeasured.

## Earlier landing-page baseline
Production build of the SvelteKit landing page (`/`) right after the migration
from TanStack Start. Use it as the comparison point for later frontend changes.

### Conditions

- Recorded 2026-10-16 on an Apple M4 Pro, macOS 27.0.1, Node 24.13.0, Bun 1.4.3.
- `bun run build`, with SvelteKit 3.1, Svelte 5.57, Vite 8.3, and `@sveltejs/adapter-node` 6.
- Sizes come from `apps/web/build/client/_app/immutable`. Gzip sizes are from
  `gzip -9 -c <file> | wc -c`. adapter-node also serves precompressed `.br`/`.gz` files.
- The document was served by `PORT=3100 node apps/web/build/index.js` on
  localhost, with the Rust API running.

### Client JavaScript

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

### Document

- SSR HTML for `/`: 2,012 B. The API health status and the `<html lang>` are
  already in the HTML before hydration.
- Time to first byte on localhost, five runs: 1.2–2.0 ms.

### Limits of the original baseline

- Browser load and hydration timings, plus client-side navigation. The app has one
  route, so there is no navigation to measure yet. Add these once the Playwright
  tooling from #5 lands.
