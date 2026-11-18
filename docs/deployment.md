# Single-host deployment

Use a Linux x86-64 host with Docker Engine 28+ and Docker Compose v2.33.1+
(or Docker Desktop for testing). Published Tikkit images target
`linux/amd64`. The same Dockerfiles can be built locally on Linux ARM64.
Keep the Compose file, its `ops/` configuration and the image pair from the
same release revision where possible. The web and API versions can differ
because their tags are independent, so use the pair tested together for the
release.

## Configure the host and secrets

1. Place [docker-compose.prod.example.yml](../docker-compose.prod.example.yml),
   [ops/caddy/Caddyfile](../ops/caddy/Caddyfile) and
   [ops/compose/init-postgres.sql](../ops/compose/init-postgres.sql) in the
   same relative layout as the repository. Use a stable Compose project name
   (`tikkit` below). The project name determines which volumes the stack uses.
2. Point the public hostname at the host IPv4 address and allow inbound TCP
   80 and 443 to Caddy.
   Caddy handles certificate issuance/renewal and redirects HTTP to HTTPS.
   Allow Caddy outbound DNS and HTTPS for certificate issuance. Set
   `PUBLIC_ORIGIN` to the actual HTTPS origin users open, with no path,
   query or trailing slash. It sets the API's links and cookie policy, the
   web adapter's fixed `ORIGIN`, and Caddy's site address. With a nonstandard
   HTTPS port, include it in the origin and set the same `HTTPS_PORT`.
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
   umask 077
   cp .env.production.example .env.production
   chmod 600 .env.production
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
   `PUBLIC_ORIGIN=https://localhost:18344` and
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

## Runtime services and probes

Caddy is the only published service. It forwards `/api/*` to
`api:3001` and strips inbound `X-Request-ID` on that entire path. It
forwards portal routes, SSR data requests, and fingerprinted
`/_app/immutable/` assets to `web:3000`. The Node image runs the generated
`apps/web/build/index.js` from the SvelteKit Node adapter, with both its
server bundle and client assets in `apps/web/build/`. It listens on
`HOST=0.0.0.0`, `PORT=3000`, and makes SSR requests via
`API_ORIGIN=http://api:3001` on the private application network.

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
show how to scrape it privately.

**Integration with #44.** The API service has a marked placeholder for the
final trusted-proxy configuration. Until that lands, the API uses its
default proxy-trust setting and can group clients behind Caddy for existing
rate limits. Wire the #44 setting for this direct Caddy hop when rebasing
the stack. Preserve the fixed external origin and request-ID stripping.

The API process runs AI attempt and sweep jobs, notification outbox
delivery, screenshot cleanup, AI usage retention and resolution-confirmation
timeouts. Jobs and deadlines
are durable in PostgreSQL. On SIGTERM the API allows 20 seconds to drain
HTTP, then up to 5 seconds each for workers and the DB pool, within the
40-second Compose stop grace period. The web adapter has a separate
20-second drain. Diagnose workers with the [operations runbook](operations.md).

To enable SMTP, allow outbound DNS and TCP from the API through the host
firewall to the configured relay port (commonly 587 for STARTTLS or 465
for implicit TLS). The Admin configures and tests the relay in the portal.
Tikkit verifies the relay certificate and stores credentials encrypted
with `ENCRYPTION_KEY`. Invitation and recovery links use `PUBLIC_ORIGIN`.
To enable AI, allow outbound DNS and HTTPS to the selected official provider
and configure its credential in Admin AI settings. Outbound configuration
is optional for both features. Mail delivery is outbound-only.

## Upgrade and rollback

Plan a maintenance window on the single host. Keep the current image digests,
version of the Compose/config files, and a verified backup. Use the same
project name and keep `postgres-data` and `screenshots-data` together.
They contain users, tickets, history, AI records/statistics, and uploads.
Keep the deployment configuration and an independent copy of the encryption
key. To make a consistent backup, quiesce application writes, take a
PostgreSQL 17 dump and archive the screenshot volume during the same
maintenance window. The reference stack exposes these backup streams:

```sh
compose exec -T postgres pg_dump -U postgres -d tikkit -Fc
compose run --rm -T --no-deps --entrypoint tar api -C /var/lib/tikkit/screenshots -cf - .
```

Pipe each stream into your encrypted backup storage. Copy the backup
off-host and restore into an isolated project with its own volumes.
Keep delivery disabled in the restore environment. See
[screenshot storage and restore](screenshots.md#storage-backup-and-restore).

1. Record the old image references and review the target's release notes and
   migration changes. Change the two image references in `.env.production`
   and pull them before taking the service offline (`compose pull api web`).
2. Stop public traffic and writers (`compose stop proxy web api`). Take the
   pre-upgrade backup before running the new migration command.
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
data changed incompatibly, restore the pre-upgrade database and screenshots
together into fresh volumes with the previous versions and secret settings
before switching traffic back. This restore loses writes after the backup.
Keep the newer volumes for investigation. Never edit migration history to
force a rollback. A PostgreSQL **major** upgrade requires a separate
database upgrade/restore procedure.

See the [release pipeline](ci.md#release-images) for component tags and
image provenance.
