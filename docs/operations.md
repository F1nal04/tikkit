# Operations runbook

Tikkit runs the notification outbox, AI attempt processor and sweeper, and
resolution-confirmation timeout worker in the API process. A self-hosted Compose
operator can use the health endpoints and service logs directly. Prometheus is
optional and uses the same database readiness check.

## API and database

- Check `/api/health` for API liveness and `/api/ready` for database and migration
  readiness. A live API with HTTP 503 on readiness reports a safe database or
  migration check code. Alert `TikkitMetricsUnreachable` also fires when the
  collector cannot reach the private listener. Check the listener address and
  local network namespace before treating a scrape failure as an API outage.
- Use `docker compose ps -a` and `docker compose logs --since 30m --tail 300 web api db`
  (substitute the service names in your stack) for startup and worker messages.
  Check the API container's exit code, restart count, and OOM status with
  `docker inspect --format '{{.State.ExitCode}} {{.RestartCount}} {{.State.OOMKilled}}' <container>`.
  A shutdown should log its drain and stop messages. A startup database error
  points to connectivity or migration history. Check `pg_isready` inside the DB
  service and review its logs locally before restarting the API. Address
  connectivity or restore the appropriate build/schema pairing. Preserve
  database and screenshot volumes.
- API logs use structured JSON by default and web logs use JSON. For an
  individual failure, search for the response's `x-request-id` (also
  `request_id` in API error envelopes). For example,
  `docker compose logs --since 30m api web | rg -F '<request-id>'`.
  A page's SSR fetches share the page request's ID. Browser-originated API
  calls have their own IDs. Durable jobs inherit the ID from their source
  event through restarts. Jobs from older events without an ID get a fresh one.
  A poll or claim failure before a job can be identified has the worker name.
- Application telemetry uses route templates, bounded error codes, latency,
  status and operational IDs. Keep logs and diagnostics private. Use a fixed
  `ORIGIN` for the Node adapter and avoid raw request URLs in reverse-proxy
  access logs, since invitation and reset URLs carry tokens.

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
  sweep worker. Look for transition failures by request ID. The sweep retries
  timed-out or orphaned AI-stage tickets after recovery. Verify falling due
  count and committed queue intake. Investigate any ticket that remains in AI
  processing after recovery through the authorized portal.

## Notifications and SMTP

- Use the Admin Email page for outbox counts, error codes, and connection
  testing. The API logs `notification delivery failed` with the stable code
  and outbox ID. SMTP failures affect email delivery independently of the
  in-app inbox and ticket work.
- For `TikkitNotificationOutboxStuck` or `TikkitNotificationEmailFailing`,
  check `notifications` polling, then verify the configured relay's outbound
  port, TLS trust, DNS, authentication, and sender policy. A scheduled
  backoff remains pending until its next eligible delivery time. Once the
  relay is repaired, let pending rows retry. Use the Admin retry action for
  rows that reached the terminal failed state. Verify that due lag and
  failures stop climbing. SMTP disabled by policy suppresses email.
- Invitation and password-reset mail diagnostics also use stable failure
  logs. The notification worker series track the durable ticket outbox.

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

## Private Prometheus dashboard and alerts

On Compose, set `METRICS_PORT=9091` on the API container, keep it unpublished,
and run the trusted collector in the API's network namespace, for example
with `network_mode: "service:api"`. Keep this namespace and the Prometheus
UI private. When recreating the API container, recreate the collector to
attach it to the same network namespace. From that namespace, a local
`curl -fsS http://127.0.0.1:9091/metrics` works without Prometheus. The API
listener and public origin expose no metrics route.

Load [alerts.yml](../ops/prometheus/alerts.yml) as a rule file. For example,
mount it read-only at `/etc/prometheus/tikkit-alerts.yml` and configure:

```yaml
rule_files:
  - /etc/prometheus/tikkit-alerts.yml
scrape_configs:
  - job_name: tikkit
    scrape_interval: 30s
    scrape_timeout: 8s
    static_configs:
      - targets: ['127.0.0.1:9091']
```

Verify the rule file and its failure/recovery examples with
`promtool check rules ops/prometheus/alerts.yml` and
`promtool test rules ops/prometheus/alerts.test.yml`.

Use the Prometheus expression browser for these graphs or paste the expressions
into panels in your private dashboard:

- Exporter, database, and snapshot health: `up{job="tikkit"}`,
  `tikkit_database_ready`, `tikkit_worker_snapshot_up`.
- API response latency: `histogram_quantile(0.95, sum by (le) (rate(tikkit_http_request_duration_seconds_bucket[5m])))`.
  Error rate: `sum by (status_class) (rate(tikkit_http_request_duration_seconds_count{status_class=~"4xx|5xx"}[5m]))`.
- Worker queue and deadlines: `tikkit_worker_pending_jobs`, `tikkit_worker_due_jobs`,
  and `tikkit_worker_oldest_due_seconds`. For AI, due means the processing
  deadline was reached or the job is orphaned. In-flight calls can run through
  their bounded grace period. For notifications, due means eligible now.
- Worker activity: `time() - tikkit_worker_last_success_timestamp_seconds`,
  `increase(tikkit_worker_failures_total[5m])`, and
  `increase(tikkit_worker_retries_total[5m])`.
- Queue intake, generation, and resolution: `increase(tikkit_ticket_operations_total[10m])`.
  The fixed `operation` labels distinguish direct intake, AI handoff,
  failed handoff, generation succeeded/failed/refused, explicit AI and human
  confirmations, timeout closures and notification email failures. A guarded
  refusal can occur before contacting a provider.

The scrape returns HTTP 200 with current process metrics when the database is
down. Use `tikkit_database_ready` and `tikkit_worker_snapshot_up` to detect the
fault. Backlog series are omitted while the snapshot is unavailable. A worker
timestamp of zero means it has yet to complete a successful poll. Counters
reset on API restart and track operations observed by that process. The
backlog gauges reflect shared database state. Metric labels contain no request
IDs, ticket identifiers, category names or requester data.
