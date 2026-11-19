# Operations runbook

## Upgrading

- Remove the legacy `TRUST_PROXY_HEADERS` setting. The API refuses to start
  when the old switch is enabled. Set `TRUSTED_PROXIES` to the subnet shared by
  Caddy and web SSR. See [configuration upgrades](configuration.md#upgrading).
- The reference Compose stack serves only the configured `PUBLIC_HOST`.
  It derives `PUBLIC_ORIGIN` from that host for browser writes.

Tikkit runs the notification outbox, AI attempt processor and sweeper,
resolution-confirmation timeout worker, and data retention worker in the API
process. A self-hosted Compose
operator can use the health endpoints and service logs directly. Prometheus is
optional and uses the same database readiness check. See the
[deployment guide](deployment.md) for installation and version changes and
[backup and restore](backup-restore.md) for the encrypted backup sidecar, its
failure alerts and the restore drill.

## API and database

- Check `/api/health` for API liveness and `/api/ready` for database and migration
  readiness. A live API with HTTP 503 on readiness reports a safe database or
  migration check code. Alert `TikkitMetricsUnreachable` also fires when the
  collector cannot reach the private listener. Check the listener address and
  local network namespace before treating a scrape failure as an API outage.
- Use `docker compose ps -a` and `docker compose logs --since 30m --tail 300 web api postgres`
  (substitute the service names in your stack) for startup and worker messages.
  Check the API container's exit code, restart count, and OOM status with
  `docker inspect --format '{{.State.ExitCode}} {{.RestartCount}} {{.State.OOMKilled}}' <container>`.
  A shutdown should log its drain and stop messages. A startup database error
  includes a stable `error_kind` and, when available, the SQLSTATE and
  migration version. Panic logs include the source file and line.
  Check `pg_isready` inside the DB
  service and review its logs locally before restarting the API. Address
  connectivity or restore the appropriate build/schema pairing. Preserve
  database and screenshot volumes.
- API logs use structured JSON by default and web logs use JSON. For an
  individual failure, search for the response's `x-request-id` (also
  `request_id` in API error envelopes). For example,
  `docker compose logs --since 30m api web | rg -F '<request-id>'`.
  A page's SSR fetches share the page request's ID. Browser-originated API
  calls have their own IDs. The public reverse proxy strips the inbound
  `X-Request-ID` on every `/api/*` request, and the private API generates an
  ID for each public call. The SSR server sends an ID directly to that API.
  Keep the API socket unpublished so public traffic passes through the proxy.
  Durable jobs inherit the ID from their source
  event through restarts. Jobs from older events without an ID get a fresh one.
  A poll or claim failure before a job can be identified has the worker name.
- Application telemetry uses route templates, bounded error codes, latency,
  status and operational IDs. Keep logs and diagnostics private. Pass the
  configured `PUBLIC_HOST` unchanged and overwrite `X-Forwarded-Proto`.
  Avoid raw request URLs in reverse-proxy access logs because invitation and
  reset URLs carry tokens.

## Worker health

Symptoms include `TikkitWorkerNotPolling` and
`TikkitWorkerSnapshotUnavailable`. Check database readiness first, then look
for the named worker in API logs. A stale poll timestamp can signal a stalled
database operation or worker even with an empty queue. Rising retries or
failures alongside a fresh poll timestamp can indicate a single failing job.
Inspect the affected worker's section below. After resolving the fault, check
that polling resumes and queue lag falls. Jobs remain in PostgreSQL across an
API restart. Avoid manual status changes or queue deletion.

## AI handoffs and provider failures

- Check `ai_attempts` and `ai_attempt_sweep` logs for stable provider outcomes
  and `ai_handoff_failed`. The provider worker can finish a generation and still
  fail to commit a result. A failed provider call can still hand off
  successfully. An increasing `queue_intake_after_ai` shows committed intake.
  Only `ai_requester_confirmed` represents a requester confirming an AI solution.
- For provider issues, inspect the Admin AI settings and usage diagnostics.
  Check credential status, outbound HTTPS/DNS, circuit breaker and budget
  codes. Review only metadata. AI disabled or exempt categories do not need a
  working provider and should route directly to the shared queue.
- For `TikkitAiHandoffFailed` or `TikkitAiAttemptStuck`, check readiness and the
  sweep worker. The failed-handoff counter records the first exhausted fallback
  or failed sweep per ticket observed by this worker process. Transient commit
  retries appear in the retry counter. Look for transition failures by request
  ID. The sweep retries
  timed-out or orphaned AI-stage tickets after recovery. Verify falling due
  count and committed queue intake. Investigate any ticket that remains in AI
  processing after recovery through the authorized portal.

## Notifications and SMTP

- The Admin Email page (`/admin/email`) tests the connection and shows the last
  delivery problem. Outbox counts and failure codes come from the Admin API,
  `GET /api/admin/notifications` (see [below](#reading-the-outbox-and-retrying-failed-email)).
  The API logs `notification delivery failed` with the stable code
  and outbox ID. SMTP failures affect email delivery independently of the
  in-app inbox and ticket work.
- For `TikkitNotificationOutboxStuck` or `TikkitNotificationEmailFailing`,
  check `notifications` polling, then verify the configured relay's outbound
  port, TLS trust, DNS, authentication, and sender policy. A scheduled
  backoff remains pending until its next eligible delivery time. Once the
  relay is repaired, let pending rows retry. Re-queue rows that reached the
  terminal failed state with `POST /api/admin/notifications/retry` (there is no
  portal button for it). Verify that due lag and
  failures stop climbing. SMTP disabled by policy suppresses email.
- For `smtp_tls_failed`, check the certificate chain and hostname. For a private
  relay, check the mounted [`SMTP_CA_FILE`](configuration.md) contains CA
  certificates. A self-signed leaf can pass startup validation and fail at
  send time. Unset the private bundle for a public relay.
- Invitation and password-reset mail diagnostics also use stable failure
  logs. The notification worker series track the durable ticket outbox.

### Reading the outbox and retrying failed email

Both calls need an Admin session and the CSRF header (the session cookie is
`HttpOnly`, so sign in with the same API). Replace `<PUBLIC_HOST>`; the
password is read without echo:

```sh
host=https://<PUBLIC_HOST>
read -rsp 'Admin password: ' pw; echo
jq -n --arg u '<admin username>' --arg p "$pw" '{username:$u,password:$p}' \
  | curl -fsS -c admin.jar -H 'X-Requested-With: tikkit' -H "Origin: $host" \
      -H 'Content-Type: application/json' --data @- "$host/api/auth/login" > /dev/null
curl -fsS -b admin.jar -H 'X-Requested-With: tikkit' "$host/api/admin/notifications"
curl -fsS -b admin.jar -X POST -H 'X-Requested-With: tikkit' -H "Origin: $host" \
  "$host/api/admin/notifications/retry"
rm admin.jar
```

The first call returns counts by status and the stable failure codes; the second
re-queues every `failed` row (audited as `notifications_retried`). Rows keep
their idempotency key, so a message whose acceptance was recorded is not sent
again (delivery is at least once across a crash, see
[notifications](notifications.md#worker)).

## Resolution confirmation timeouts

For `TikkitConfirmationTimeoutStuck`, inspect `auto_close` poll, failure, retry
and timeout closure telemetry. Its pending jobs are staff proposals with a
snapshotted deadline. Its due jobs have reached that deadline. Lag is measured
from the oldest due deadline. Fresh polls alongside growing overdue lag can
mean a transition is failing and being deferred for the next drain cycle.
Check database health and use the request ID to trace back to the proposal.
After recovery, check that overdue count falls and
`confirmation_timeout_closed` increases. Requester-confirmed closure has
its own counter. This worker handles the resolution-confirmation policy only.

## Data retention

The `retention` worker applies Admin-started and daily automatic retention
runs ([retention.md](retention.md)). It is idle unless a run is pending; a
pending run is its one pending and due job, and
`tikkit_worker_oldest_due_seconds{worker="retention"}` is the run's age. A run
works in batches and survives restarts, so a long run on a large database is
normal. For `TikkitRetentionRunStuck`, check database readiness, then the
`retention` log lines by the run's request ID (the `x-request-id` of the Admin
request that started it, also on its `retention_run_started` audit event). A
batch that fails rolls back and is retried on the next poll; the run's counts
on the Admin page stop growing while it fails. A run also stays pending while
expired records are locked by other transactions: it logs `expired records
are locked by other transactions`, backs off (up to 10 minutes), and tries
again. Both cases increment `tikkit_worker_retries_total{worker="retention"}`.
A long-lived transaction that holds a ticket row is the usual cause; find it
in `pg_stat_activity`. During a backup that holds the screenshot file lock,
runs continue and only their file deletions are deferred. Never edit `retention_runs` or
delete screenshot files by hand: a file whose row is gone is removed by the
hourly screenshot clean-up. After restoring a backup, start a run again
([retention.md](retention.md#backups)).

## Private Prometheus dashboard and alerts

Apply the [optional Compose overlay](../docker-compose.observability.example.yml)
to the production Compose file to set `METRICS_PORT=9091`, start Prometheus,
and load the [scrape configuration](../ops/prometheus/prometheus.yml) and
[alert rules](../ops/prometheus/alerts.yml).

```sh
docker compose --env-file .env.production -p tikkit -f docker-compose.prod.example.yml \
  -f docker-compose.observability.example.yml up -d
```

Prometheus shares the API network namespace via `network_mode: "service:api"`.
It scrapes `127.0.0.1:9091/metrics` and binds its own HTTP listener to loopback
with `--web.listen-address=127.0.0.1:9090`. Keep both ports unpublished and
outside public proxy routes. A wildcard Prometheus bind would expose its UI
and metrics data to other containers on networks shared with the API.
When recreating the API container, recreate the collector to attach it to the
same network namespace. To query from inside that namespace, use
`promtool query instant http://127.0.0.1:9090 'up{job="tikkit"}'` in the
Prometheus container, or fetch `http://127.0.0.1:9091/metrics` with a local HTTP
client to inspect the exporter directly.

Verify the rules locally with the same pinned image as CI:

```sh
image=prom/prometheus:v3.5.0@sha256:63805ebb8d2b3920190daf1cb14a60871b16fd38bed42b857a3182bc621f4996
promtool() {
  docker run --rm --network none --entrypoint /bin/promtool \
    -v "$PWD/ops/prometheus:/rules:ro" -w /rules "$image" "$@"
}
promtool check rules alerts.yml
promtool test rules alerts.test.yml
```

Use the Prometheus expression browser for these graphs or paste the expressions
into panels in your private dashboard:

- Exporter, database, and snapshot health: `up{job="tikkit"}`,
  `tikkit_database_ready`, `tikkit_worker_snapshot_up`.
- API response latency: `histogram_quantile(0.95, sum by (le) (rate(tikkit_http_request_duration_seconds_bucket[5m])))`.
  Error rate: `sum by (status_class) (rate(tikkit_http_request_duration_seconds_count{status_class=~"4xx|5xx"}[5m]))`.
- Worker queue and deadlines: `tikkit_worker_pending_jobs`, `tikkit_worker_due_jobs`,
  and `tikkit_worker_oldest_due_seconds`. The AI scrape and sweeper share the
  same eligibility calculation. An orphan is due immediately. An attempt
  without a live claim is due after its deadline or when its claim budget is
  exhausted. A live claim gets until its lease expires or the hard limit of
  deadline plus configured concurrency wait, provider call timeout, and sweep
  margin, whichever first makes it eligible. Lag starts at that eligibility
  time. For notifications, due means eligible now.
- Worker activity: `time() - tikkit_worker_last_success_timestamp_seconds`,
  `increase(tikkit_worker_failures_total[5m])`, and
  `increase(tikkit_worker_retries_total[5m])`.
- Queue intake, generation, and resolution: `increase(tikkit_ticket_operations_total[10m])`.
  The fixed `operation` labels distinguish direct intake, AI handoff,
  failed handoff, generation succeeded/failed/refused, explicit AI and human
  confirmations, timeout closures and notification email failures. A terminal
  generation refusal is counted once when the attempt's handoff commits.
  Waiting for a provider slot increments retries.

The scrape returns HTTP 200 with current process metrics when the database is
down. Use `tikkit_database_ready` and `tikkit_worker_snapshot_up` to detect the
fault. Backlog series are omitted while the snapshot is unavailable. A worker
timestamp of zero means it has yet to complete a successful poll. Counters
reset on API restart and track operations observed by that process. The
backlog gauges reflect shared database state. Metric labels contain no request
IDs, ticket identifiers, category names or requester data.

## Load and recovery drill

Use the [capacity profile](performance-baseline.md) and the
[disposable Compose runner](../ops/load/README.md) to rehearse recovery.
During a database outage, [readiness](#api-and-database) should fail while API
liveness stays up. After connectivity returns, verify both readiness and
[worker polling](#worker-health). Retry an interrupted submission with the
original idempotency key and input.

For a stuck AI attempt, watch [AI handoffs](#ai-handoffs-and-provider-failures)
and confirm that the requester can still submit through an exempt category.
For delayed email, check [outbox retries and SMTP](#notifications-and-smtp).
A relay acceptance followed by a lost response or a process crash before the
delivery receipt commits is an ambiguous SMTP outcome. A durable receipt
suppresses retries, but the relay can deliver twice inside that gap.

After an API restart, check [overdue confirmations](#resolution-confirmation-timeouts)
and [retention runs](#data-retention) for catch-up. The drill starts two API
replicas briefly to exercise concurrent workers against the same data.
Operate one API replica in production because the per-account and sign-in
rate limits live in each process.

## Backups

The optional backup sidecar logs `level=error` lines, reports health in
`docker compose ps`, and exposes `tikkit_backup_*` metrics to the same private
Prometheus. Apply the
[backup alert overlay](../docker-compose.backup-alerts.example.yml) beside the
observability overlay to load the `TikkitBackupFailed`, `TikkitBackupStale`,
`TikkitBackupMissingScreenshots` and `TikkitBackupSidecarDown` rules. The main
rules include `TikkitRestoreModeLeftOn`: a restored copy runs no jobs and sends
nothing, so it must not stay in restore mode for more than a few hours. Their
causes and fixes are in [failure alerts](backup-restore.md#failure-alerts).
