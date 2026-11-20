# Single-host deployment

Use a Linux x86-64 host with Docker Engine 28+ and Docker Compose v2.33.1+
(or Docker Desktop for testing). Published Tikkit images target
`linux/amd64`. The same Dockerfiles can be built locally on Linux ARM64.
Keep the Compose file, its `ops/` configuration and the image pair from the
same release revision where possible. The web and API versions can differ
because their tags are independent, so use the pair tested together for the
release.

## Size the host

Use 4 vCPU and 8 GiB RAM as the starting size for a single-organization
deployment close to the [measured capped reference](performance-baseline.md#capped-reference-rerun-results)
of 30 active workload lanes and 30,000 historical tickets. The reference
allocated 4 vCPU and 4 GiB of container memory across the stack and local test
peers. It ran on an ARM developer workstation with container caps and a
co-located load driver. Treat that result as sizing guidance for this workload
and measure storage separately for your attachment and backup retention.

## Configure the host and secrets

1. Place [docker-compose.prod.example.yml](../docker-compose.prod.example.yml),
   [ops/caddy/Caddyfile](../ops/caddy/Caddyfile) and
   [ops/compose/init-postgres.sql](../ops/compose/init-postgres.sql) in the
   same relative layout as the repository. Use a stable Compose project name
   (`tikkit` below). The project name determines which volumes the stack uses.
   Keep the mounted `ops/caddy/Caddyfile` and `ops/compose/init-postgres.sql`
   world-readable (`chmod 644`) so Caddy (UID 10001) and PostgreSQL (UID 999)
   can read them. The mounted files contain configuration and no secrets.
2. Point the public hostname at the host IPv4 address and allow inbound TCP
   80 and 443 to Caddy.
   Caddy handles certificate issuance/renewal and redirects HTTP to HTTPS.
   Allow Caddy outbound DNS and HTTPS for certificate issuance. Set
   `PUBLIC_HOST` to the lowercase hostname users open, without a scheme, path,
   trailing slash or default port. Compose uses it as Caddy's site authority
   and derives `PUBLIC_ORIGIN=https://<PUBLIC_HOST>` for the API's links, cookies,
   and CSRF checks. With a nonstandard HTTPS port, include it in `PUBLIC_HOST`
   and set the same `HTTPS_PORT`. Browser mutations require that exact origin
   and `Sec-Fetch-Site: same-origin` when the browser sends it.
3. Copy the [production env example](../.env.production.example) to
   `.env.production` and restrict it to the deployment operator.
   Set `TIKKIT_WEB_IMAGE` to
   `ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z` and `TIKKIT_API_IMAGE` to
   `ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z`, using their real release
   versions. You can also supply full `image@sha256:…` references.
   Public packages can be pulled anonymously. If packages are private, use
   the host's Docker credential store and a registry login with package read
   access only.

   ```sh
   (umask 077; cp .env.production.example .env.production; chmod 600 .env.production)
   ```

   Generate each value independently and put it in the protected env file or
   inject it from your host's secret manager.

   - `POSTGRES_PASSWORD` is the bootstrap administrator password.
     `TIKKIT_DB_PASSWORD` is the separate application login password. Use
     `openssl rand -hex 32` for each. The app password must be URL-safe hex
     because Compose uses it in `DATABASE_URL`.
   - `SESSION_SECRET` and `ENCRYPTION_KEY` each use
     `openssl rand -base64 32`. Keep the encryption key stable and back it up
     separately. It is needed to read stored AI and SMTP credentials after a restore.

   The env file is git-ignored. Restrict host and Docker access because
   Docker administrators can inspect container environment variables.
   PostgreSQL initializes these roles only on an empty data volume. Changing
   the env file alone leaves the existing database passwords in effect.
   Rotate them in PostgreSQL and update the environment together during
   maintenance.

4. For initial setup, restrict client access until the first Admin exists.
   For a private bootstrap or test, use `PROXY_BIND=127.0.0.1`,
   `HTTP_PORT=18380`, `HTTPS_PORT=18344`,
   `PUBLIC_HOST=localhost:18344` and
   `CADDY_GLOBAL_OPTIONS=local_certs`. Connect locally or tunnel port 18344 to
   the operator workstation. Export and trust only the local Caddy root
   certificate for the bootstrap client. When moving to public TLS, restore
   the public origin, ports and bind address, clear `CADDY_GLOBAL_OPTIONS`,
   then run `compose up -d --wait` and sign in at the public origin.

## Start, migrate, and check readiness

Run these commands from the deployment directory.

```sh
compose() {
  docker compose --env-file .env.production -f docker-compose.prod.example.yml -p tikkit "$@"
}

compose pull
compose up -d --wait postgres
compose run --rm --no-deps api migrate
compose up -d --wait
compose ps
curl -fsS https://help.example.com/api/health
curl -fsS https://help.example.com/api/ready
```

`api migrate` runs the API image's embedded SQLx migrations, closes the
database pool and exits before HTTP and worker startup.
It is safe to rerun with the same image. Normal API startup also applies
those migrations before starting the listener, using the same migrator and
application DB login. The explicit command makes migration failures visible
before releasing traffic. If it fails, inspect the redacted diagnostics and
resolve the issue before starting the services.

For the private TLS example, copy the CA after start and verify the actual
HTTPS endpoint.

```sh
compose cp proxy:/data/caddy/pki/authorities/local/root.crt ./tikkit-local-ca.crt
curl --cacert ./tikkit-local-ca.crt -fsS https://localhost:18344/api/ready
```

Open `/setup` on the portal origin and create the first Admin. Setup then
closes permanently. Sign in and use Admin Invitations to copy links for
requesters and staff. Configure your teams and intake catalog before
accepting tickets. AI and SMTP start disabled and require no provider key
or relay credentials for setup, readiness or ticket handling.

Before accepting real tickets, set up [encrypted backups and an off-host
copy](backup-restore.md) and run the restore drill once. The deployment is
not recoverable without them, and the backup decryption key and
`ENCRYPTION_KEY` must be stored apart from the host.

### PostgreSQL initialized without the application login

If PostgreSQL started but stays unhealthy, inspect `compose logs postgres`.
A permission error reading `10-tikkit.sql` can leave an initialized data
volume without the `tikkit` login or database. The upstream initializer only
runs on an empty volume, so fixing the mount and restarting alone leaves
that failure in place. Fix the file mode, then inspect the cluster:

```sh
compose exec -T postgres psql -U postgres -d postgres -c '\du'
compose exec -T postgres psql -U postgres -d postgres -c '\l'
```

For a failed fresh install, use a fresh project and volumes, or, after
confirming both the login and database are absent, run the initializer once
against the running PostgreSQL container:

```sh
compose exec -T postgres psql -v ON_ERROR_STOP=1 -U postgres -d postgres \
  < ops/compose/init-postgres.sql
compose up -d --wait postgres
```

If either object exists, inspect the partial state and preserve the volume
until you have established what it contains.

## Runtime services and probes

Caddy is the only published service. It rejects other HTTPS authorities with
HTTP 421 and forwards the configured `Host` unchanged. It derives
`X-Forwarded-For` and `X-Forwarded-Proto` from the public connection, strips
inbound `X-Request-ID` on API routes, and forwards those routes to `api:3001`.
There is no Caddy `trusted_proxies` setting in the reference stack.

The web service receives portal routes, SSR data requests and fingerprinted
`/_app/immutable/` assets on `web:3000`. It runs the Node adapter's generated
`apps/web/build/index.js`, with the server bundle and client assets in
`apps/web/build/`. It listens on `HOST=0.0.0.0` and `PORT=3000`. The adapter
uses `PROTOCOL_HEADER=x-forwarded-proto` and the original `Host` to form the
page origin. Adapter-node 6 ignores the old `ORIGIN` variable. SSR uses
`API_ORIGIN=http://api:3001` for same-origin API calls and forwards the
client address from Caddy.

Caddy sets one-year HSTS on HTTPS responses and fills missing `nosniff`
and `frame-ancestors 'none'` headers, including on static assets and proxy
errors. Pages retain their nonce-based CSP. The proxy bounds header reads to
10 seconds, request bodies to 45 seconds, and idle connections to 2 minutes.
Its 10 MiB body cap matches the screenshot upload limit. The upstream
response-header timeout is 75 seconds, allowing the API's 60-second request
deadline to expire first.

The database has its own internal network with the API, and uses the pinned
PostgreSQL 17 image. Its probe connects over TCP with the application login
after cluster initialization. The API probe checks both `/api/health`
(process liveness) and `/api/ready` (database and exact migration history).
The web probe asks the Node server for the setup page or its redirect.
Caddy has a loopback-only process check. Probe the public origin separately
to verify certificate trust and end-to-end routing. Compose waits on
dependency probes for startup and reports health in `compose ps`. The
restart policy restarts exited processes. An unhealthy result alone leaves
the process running for diagnosis.

Every service uses a non-root UID, a read-only root filesystem, and no Linux
capabilities. Writable storage is limited to its volume or temporary
mounts. Only the API mounts private screenshots. Caddy persists its TLS
state in `caddy-data`. The API can reach the internet via a separate egress
bridge and has no published socket. Keep the API, PostgreSQL and metrics
off the host's public listeners. Optional `METRICS_PORT=9091` binds to
the API container's loopback, and the [observability overlay and runbook](operations.md)
show how to scrape it privately. The optional collector shares the API
network namespace, so treat it as part of the API trust boundary.

The internal `app` network is `172.30.0.0/24`. Its only members are Caddy,
web and the API. The API's `TRUSTED_PROXIES` matches this subnet because both
Caddy and web SSR forward client addresses. If it overlaps a host network,
choose a free private subnet and update both places in Compose. Keep the
web and API ports unpublished. See [proxy trust and rate limits](security.md#reverse-proxy-trust).

Run one API replica. Per-account and sign-in rate limits are held in memory
per API process and reset on restart. The API also hosts AI attempts and
sweeps, outbox delivery, screenshot cleanup, resolution-confirmation timeouts,
AI usage cleanup, and the data-retention worker. Jobs and deadlines are
durable in PostgreSQL. On SIGTERM the API allows 20 seconds to drain HTTP,
then up to 5 seconds each for workers and the DB pool, within the 40-second
Compose stop grace period. The web adapter has a separate 20-second drain.
See the [operations runbook](operations.md) for worker diagnostics.

Admins can inspect the [audit log](audit.md) and configure policy in
`/admin/retention`. The [retention worker](retention.md#defaults) deletes
nothing by default. Automatic runs are off, and all its periods are set to
keep indefinitely. Audit-log retention has a 30-day minimum when configured.
The existing orphan-upload and AI usage cleanup policies still apply.

To enable SMTP, allow outbound DNS and TCP from the API through the host
firewall to the configured relay port (commonly 587 for STARTTLS or 465
for implicit TLS). The Admin configures and tests the relay in the portal.
Tikkit verifies the relay certificate and stores credentials encrypted
with `ENCRYPTION_KEY`. For a private relay, mount a PEM bundle of CA certificates
read-only and set `SMTP_CA_FILE` in the API service's explicit `environment`
list. The production Compose example shows both entries. Adding it only to the
Compose env file has no effect. This setting replaces the default WebPKI roots.
Unset it when switching to a public relay. See [SMTP trust errors](operations.md#notifications-and-smtp).
Invitation and recovery links use `PUBLIC_ORIGIN`.
To enable AI, allow outbound DNS and HTTPS to the selected official provider
and configure its credential in Admin AI settings. Outbound configuration
is optional for both features. Mail delivery is outbound-only.

## Upgrade and rollback

When upgrading from the old proxy configuration, remove
`TRUST_PROXY_HEADERS` from your environment and overrides. The reference stack
sets `TRUSTED_PROXIES=172.30.0.0/24`. If you customized the network, set
`TRUSTED_PROXIES` to your exact private subnet. Set `PUBLIC_HOST` in
the deployment env file so Compose derives the API origin.
If an existing project's `app` network needs a new subnet, stop writes
and back up first, then use `compose down` without volume-removal flags.
Start PostgreSQL with `compose up -d --wait postgres` before applying migrations.

Plan a maintenance window on the single host. Keep the current image digests,
version of the Compose/config files, and a verified backup. Use the same
project name and keep `postgres-data` and `screenshots-data` together.
They contain users, tickets, history, AI records/statistics, and uploads.
Keep the deployment configuration and an independent copy of the encryption
key. Take a verified backup before every upgrade with the
[encrypted backup sidecar](backup-restore.md#set-up-scheduled-backups): run it
on demand (`exec backup /opt/tikkit-backup/entrypoint.sh run`) and copy the
result off the host. It dumps PostgreSQL and then archives the screenshot
volume with the non-secret configuration, so the two stay consistent
without stopping the application. Restore into an isolated project with
[ops/backup/restore.sh](backup-restore.md#isolated-restore), and see
[screenshot storage and restore](screenshots.md#storage-backup-and-restore).

1. Record the old image references and review the target's release notes and
   migration changes. Change the two image references in `.env.production`
   and pull them before taking the service offline (`compose pull api web`).
2. Stop public traffic and writers (`compose stop proxy web api`). Take the
   pre-upgrade backup before running the new migration command (a final
   on-demand backup after the writers stopped has no recovery-point gap).
3. Run `compose run --rm --no-deps api migrate`. On success run
   `compose up -d --wait`, verify both public probes, and check sign-in,
   existing ticket history, and a screenshot. Recreate the Prometheus
   collector if it shares the API container's network namespace.
4. Keep the previous images and backup until the upgrade is verified. Never
   remove volumes when upgrading, recreating containers, or changing images.

Migrations are forward-only and checked by version and checksum. Make
changes additive first and defer data/schema removals until all readers and
writers support them. Even an additive migration changes the history, so
an old API with a different embedded migration set fails startup/readiness
against the new schema. Keep old API instances stopped during the migration
and use a compatible frontend/API pair when bringing traffic back.

If the migration set and stored data remain compatible with the previous
release, stop the app services, restore the previous image references and
deployment configuration, run the previous image's `api migrate` as a
compatibility check, then start and probe it. If migrations advanced or
data changed incompatibly, restore the pre-upgrade backup (database and screenshots
together) into fresh volumes with the previous versions and secret settings
before switching traffic back. This restore loses writes after the backup.
Keep the newer volumes for investigation. Never edit migration history to
force a rollback. A PostgreSQL **major** upgrade requires a separate
database upgrade/restore procedure.

### Restore to fresh volumes

Use [ops/backup/restore.sh](../ops/backup/restore.sh) with the encrypted backup
from the same run: it restores the database and the screenshot files together
into a new, isolated Compose project, verifies checksums and consistency, and
starts the copy in restore mode, so nothing is mailed or sent to an AI provider.
The full procedure, key custody, the post-restore checklist and the recovery
objectives are in [Backup, restore and disaster recovery](backup-restore.md).
Never edit migration history to force a rollback, and keep the newer volumes
for investigation.

See the [release pipeline](ci.md#release-images) for component tags and
image provenance. Release Please creates the tags from `main`. The image
workflow publishes after CI passes and the `release` environment admits the job. The first hosted
release and GHCR publication are still to be verified.
