# Capacity and recovery tests

Use a disposable host or the shared test machine. The runner owns the
`tikkit-m4-50` project, loopback ports 18744/18780, and private application and
provider subnets. It refuses an occupied project, ignores inherited Compose
settings, takes the shared e2e lock for seeding and measurement, and removes its
containers and volumes on exit. Run one mode at a time. A second API replica,
when needed, runs in the same project and shares its database.

Build the images from the revision under test:

```sh
docker build -f apps/api/Dockerfile -t tikkit-m4-50-api:local .
docker build -f apps/web/Dockerfile -t tikkit-m4-50-web:local .
export LOAD_API_IMAGE=tikkit-m4-50-api:local LOAD_WEB_IMAGE=tikkit-m4-50-web:local
```

Docker Compose v2 with `!override` support, Bun, Bash, jq, curl, OpenSSL and
GNU `timeout` are required. For the browser measurement also install the
repository dependencies and Playwright Chromium:

```sh
bun install --frozen-lockfile
(cd apps/web && bunx playwright install chromium)
```

## Run

- `ops/load/run.sh profile > profile.json` starts from 30,000 historical
  tickets across 300 requester accounts. It uses 30 concurrent lanes for 120 s.
- `ops/load/recovery.sh > recovery.json` injects faults on a fresh seeded
  stack and records the outcomes. Use at least 61 requester accounts. Allow
  several minutes for the crash lease and actual SMTP backoff.
- `ops/load/run.sh frontend > frontend.json` uses Playwright and the browser
  performance APIs against the production web image and the seeded database.
- `ops/load/bundle-budget.sh "$LOAD_WEB_IMAGE"` checks total gzip-compressed
  client JavaScript in the actual production image.
- `ops/load/run.sh smoke` uses two lanes, 12 requesters, 12 historical tickets,
  and five seconds. The existing Compose CI job enables the private SMTP
  probe and checks the bundle budget. Full profile, fault injection and browser timing are manual.

Optional environment overrides:

| Variable | Profile default | Purpose |
| --- | ---: | --- |
| `LOAD_USERS` | 30 | Concurrent workload lanes, each with one operator and an independent claim contender |
| `LOAD_ACCOUNTS` | 300 | Requester accounts from which the lanes rotate |
| `LOAD_TICKETS` | 30,000 | Historical tickets, aged at regular intervals |
| `LOAD_SECONDS` | 120 | Scheduling window, excluding seeding and final verification |
| `LOAD_PROVIDER_MS` | 250 | Deterministic stub response delay in milliseconds, before failure scenarios override it |
| `LOAD_SMTP` | 1 | Enable capture SMTP. Defaults to 0 for smoke and frontend runs |

## Workload and assertions

Bun is the HTTP load driver. It is already a repository prerequisite and can
coordinate cookie sessions, paired writes and cross-endpoint assertions
directly. Playwright measures browser rendering. PostgreSQL's existing
`psql`, the API's private Prometheus endpoint and Docker CLI supply diagnostics.

The seed bootstraps through the real Admin API and inserts only historical
fixtures with SQL. Tickets have frozen intake, varied categories/priorities,
team assignment, immutable events, submission receipts, public replies and
internal notes. Historical notification fan-out is intentionally omitted. New
tickets, screenshots, messages, outbox and AI records use normal API and worker
paths. Credentials and trust material are generated in a private temporary
directory and removed with the stack.

Lanes sign in with independent accounts and pace cycles to stay within the
product budgets. They read requester history and detail, paginate/filter/search
the staff queue, read staff detail, and concurrently replay submissions.
One in five submissions includes a synthetic 1280 × 720 PNG. One in six requests
uses the non-exempt category and goes through the deterministic HTTPS provider.
The other category goes straight to the general queue. Eligible human
workflows dispatch, race two claims, post a reply and a private note, propose a
resolution, and race two requester confirmations. One lane polls the Admin
report, submission list and audit log.

An operator session probes a permanently AI-solved sentinel during load.
Operators also test newly solved records against queue list/search, detail,
messages, activity, notification inbox and screenshot reads. The sentinel
checks that the AI decision leaves queue counts unchanged. Report totals and
live database counts are compared to acknowledged actions, including immutable
events, submission keys, messages, screenshots and notification receipts.

API egress is restricted to internal Docker networks. The provider stub owns
the official provider hostname on a private bridge and uses a disposable
certificate. The mail test similarly uses verified implicit TLS against a
capture relay. Raw mail and provider payloads are never written to disk. The
relay exposes only counts and transient payload digests to the assertions. A
successful run outputs aggregate JSON to stdout. Keep stderr local for
diagnostics.

See the [release targets and measured results](../../docs/performance-baseline.md)
and the [operations runbook](../../docs/operations.md#load-and-recovery-drill).
