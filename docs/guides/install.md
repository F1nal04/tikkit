# Install Tikkit with Docker Compose

This is the working command sequence for a fresh single-host install: data
services, migrations, the first Admin, public TLS routing, backups, upgrades,
rollback limits and restore. It gives the commands in order and links to the
reference pages for detail:
[deployment](../deployment.md), [backup and restore](../backup-restore.md),
[operations](../operations.md), [configuration](../configuration.md).


## Before you start

You need:

- A Linux x86-64 host with Docker Engine 28 or newer and Docker Compose
  v2.33.1 or newer. Docker Desktop works for testing. Published images are
  `linux/amd64`.
- `openssl` and `curl`. For restores and the restore drill you also need `jq`.
- A DNS name for the portal (for example `help.<your-domain>`) pointing at the
  host's IPv4 address, with inbound TCP 80 and 443 open. Caddy, the bundled
  reverse proxy, obtains and renews the TLS certificate itself and needs
  outbound DNS and HTTPS to do so.
- Outbound access only for what you turn on later: your SMTP relay port
  (587 or 465) and the AI provider's HTTPS host.
- A second place to keep secrets and backups that is not this host (a password
  manager and an off-host storage target).

Nothing else is required. SMTP and AI are optional and start disabled; the
service desk works fully without them.

## 1. Get the files and the images

> **No release has been published yet.** There is no release tag and no image in
> the container registry at the time of writing, so the registry path below does
> not work today. Build the images yourself from the commit you want to run. The
> registry path is the one to use once releases exist.

Use a checkout of the repository as the deployment directory. It holds the
Compose file, `ops/` configuration and the backup tooling, and they must match the
images you run.

**Today: build both images from your checkout.**

```sh
git clone https://github.com/F1nal04/tikkit.git
cd tikkit
git checkout <the commit you want to run>      # for example the tested tip of main
docker build -f apps/api/Dockerfile -t tikkit-api:local .
docker build -f apps/web/Dockerfile -t tikkit-web:local .
```

In step 2 set `TIKKIT_API_IMAGE=tikkit-api:local` and
`TIKKIT_WEB_IMAGE=tikkit-web:local`, and **skip every `compose pull`** in this guide:
local tags are not in a registry and `pull` fails for them. The first build compiles the
Rust API and takes a while.

**Once releases exist:** check out the release tag (`api-vX.Y.Z`) and use the
published pair. The web and API images have independent version tags
(`ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z`, `ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z`);
take the pair listed together in the release notes, or pin images by digest. Release
images are published by the release workflow ([release images](../ci.md#release-images)).
Then `compose pull` applies.

## 2. Configure the environment and secrets

Create the protected environment file and fill the four secrets with generated
values. The commands below never print them.

```sh
(umask 077; cp .env.production.example .env.production)
for name in POSTGRES_PASSWORD TIKKIT_DB_PASSWORD; do
  sed -i.bak "s|^$name=.*|$name=$(openssl rand -hex 32)|" .env.production
done
for name in SESSION_SECRET ENCRYPTION_KEY; do
  sed -i.bak "s|^$name=.*|$name=$(openssl rand -base64 32)|" .env.production
done
rm .env.production.bak
```

Then edit `.env.production` and set:

| Variable | Value |
| --- | --- |
| `PUBLIC_HOST` | The lowercase hostname users open, for example `help.<your-domain>`. No scheme, path, trailing slash or `:443`. For a nondefault HTTPS port include it (`help.<your-domain>:8443`) and set the same `HTTPS_PORT`. Compose derives the API's `PUBLIC_ORIGIN` as `https://<PUBLIC_HOST>`. |
| `TIKKIT_WEB_IMAGE`, `TIKKIT_API_IMAGE` | The two image references from step 1: your local tags today; later a version tag or an `image@sha256:…` digest. |

What the secrets are for:

- `POSTGRES_PASSWORD` and `TIKKIT_DB_PASSWORD` are the database administrator
  and application passwords. PostgreSQL sets them only when it first
  initializes an empty volume. Changing the file later does not change an
  existing database ([deployment](../deployment.md#configure-the-host-and-secrets)).
- `SESSION_SECRET` must be set; nothing stored depends on its value.
- `ENCRYPTION_KEY` encrypts the stored AI and SMTP credentials. **Keep a copy
  outside this host and outside your backups.** Without it those credentials
  cannot be read after a restore (ticketing keeps working; you enter them again).

`.env.production` is git-ignored and owner-only. Docker administrators can read
container environments, so restrict Docker access to the operators.

The reference network has the fixed subnet `172.30.0.0/24`, and the API's
`TRUSTED_PROXIES` matches it. If that range collides with a network on your
host, change both in `docker-compose.prod.example.yml`
([deployment](../deployment.md#runtime-services-and-probes)).

## 3. Know what is persistent

Compose creates named volumes under the project name (`tikkit` below). Always
use the same project name; it decides which volumes the stack uses.

| Volume | Holds | Lost means |
| --- | --- | --- |
| `postgres-data` | Accounts, tickets, history, audit log, AI records, settings, encrypted credentials | All data |
| `screenshots-data` | Private PNG/JPEG/WebP screenshots | Screenshots (rows remain, previews fail) |
| `caddy-data` | TLS certificates and account state | Certificates are issued again |

Never run `down -v`, remove volumes, or change the project name while
upgrading. Back up `postgres-data` and `screenshots-data` together
([step 8](#8-set-up-backups-before-real-tickets)).

## 4. Start the stack in order

### Decide first who can reach the portal

**Until the first Admin exists, whoever reaches `/setup` first becomes the Admin.**
There is no bootstrap secret. Starting the stack publicly with open ports 80/443
exposes `/setup` to the internet, so choose how to keep it closed *before* you bring
anything up:

- **Firewall.** Allow only your own address to ports 80/443 until setup is done,
  and use the public `PUBLIC_HOST`.
- **Private bootstrap (loopback).** Set these variables in `.env.production`
  (replace the existing `PUBLIC_HOST` line, add the others):

  ```sh
  PUBLIC_HOST=localhost:18344
  PROXY_BIND=127.0.0.1
  HTTP_PORT=18380
  HTTPS_PORT=18344
  CADDY_GLOBAL_OPTIONS=local_certs
  ```

  Reach it from your workstation with an SSH tunnel
  (`ssh -L 18344:127.0.0.1:18344 <host>`) or locally.

### Start

Run everything from the deployment directory. Define this helper once per shell:

```sh
compose() {
  docker compose --env-file .env.production -p tikkit -f docker-compose.prod.example.yml "$@"
}
```

Start the database, apply migrations, then start everything. With registry images
run `compose pull` first; with locally built images do not.

```sh
compose up -d --wait postgres
compose run --rm --no-deps api migrate
compose up -d --wait
compose ps
```

`api migrate` applies the embedded migrations and exits before any HTTP or
worker start, so a migration failure shows up before traffic is possible. It is
safe to rerun. The API also migrates itself on start, with the same migrator.
Migrations are forward-only (see [rollback limits](#rollback-limits)).

Check liveness and readiness through the proxy (for the private bootstrap, first
export Caddy's local root certificate and trust it for the bootstrap client only):

```sh
compose cp proxy:/data/caddy/pki/authorities/local/root.crt ./tikkit-local-ca.crt   # private bootstrap only
curl --cacert ./tikkit-local-ca.crt -fsS https://localhost:18344/api/ready          # private bootstrap
curl -fsS https://<PUBLIC_HOST>/api/health      # {"status":"ok"}
curl -fsS https://<PUBLIC_HOST>/api/ready       # status ready, database ok, migrations ok
```

Do not run more than one API replica; rate limits and workers assume one
process ([deployment](../deployment.md#runtime-services-and-probes)).

## 5. Create the first Admin

Open `https://<PUBLIC_HOST>/setup` (for the private bootstrap
`https://localhost:18344/setup`). Enter a username, a contact email, a
password of at least 12 characters and a language, then choose **Create
administrator**. You are signed in as the first Admin. Setup then closes
permanently; a later `/setup` answers that setup has already been completed.

Set the interface language from the language selector at any time; the product
is available in English and German.

### Move from the private bootstrap to the public origin

When the first Admin exists and DNS points at the host:

1. In `.env.production` set `PUBLIC_HOST` back to your public hostname, remove
   `PROXY_BIND`, `HTTP_PORT`, `HTTPS_PORT` and `CADDY_GLOBAL_OPTIONS`.
2. Run `compose up -d --wait`.
3. Open `https://<PUBLIC_HOST>` and sign in again. Cookies and the CSRF check
   are tied to the public origin, so the portal must be opened at exactly that
   address. A save from any other address (an IP, another hostname) is
   refused with `csrf_rejected`
   ([troubleshooting](troubleshooting.md#every-save-is-refused-with-csrf_rejected)).

Caddy redirects HTTP to HTTPS, rejects other hostnames with HTTP 421 and adds
the security headers. The web and API ports are never published.

## 6. Configure the organization

Do this as the Admin before inviting everyone; the order matters because
requesters cannot submit until a category and a priority level exist.
[The administrator guide](admin.md#first-day-checklist) has the checklist:
organization settings, ticket catalog, accounts and invitations, teams.

SMTP and AI are optional and can be added at any time afterwards:

- Outbound email: [admin.md](admin.md#outbound-email). Allow the API's outbound
  DNS and TCP to the relay port.
- AI assistance: [ai.md](ai.md). Allow outbound DNS and HTTPS to the chosen
  provider.

## 7. Logs and health

```sh
compose ps                                         # health of every service
compose logs --since 30m --tail 300 web api postgres
curl -fsS https://<PUBLIC_HOST>/api/ready
```

API logs are JSON by default; every API response carries `x-request-id`, which
you can search in the logs. Probes, worker diagnostics and the optional
Prometheus overlay are in the [operations runbook](../operations.md).
Symptoms and fixes for a fresh install are in
[troubleshooting](troubleshooting.md).

## 8. Set up backups before real tickets

A deployment without backups is not recoverable. The optional sidecar writes
encrypted, consistent backups of the database, the screenshots and the
non-secret configuration, online and without stopping anything.

1. On a trusted machine (not the host), build the backup image and create a key
   pair. Store the private key in your password manager or offline storage and
   delete the local copy:

   ```sh
   docker build -f ops/backup/Dockerfile -t tikkit-backup:local .
   (umask 077; docker run --rm tikkit-backup:local age-keygen > tikkit-backup-key.txt)
   grep 'public key' tikkit-backup-key.txt
   ```

2. On the host, add the **public** key to `.env.production`:

   ```sh
   BACKUP_AGE_RECIPIENTS=<age public key>
   ```

   Optional: `BACKUP_TIMES_UTC` (default `03:15`), `BACKUP_RETENTION_DAYS`
   (14), `BACKUP_MIN_KEEP` (3), `BACKUP_VOLUME` (a host path on another disk).

3. Start the sidecar. From now on use this helper, which includes the overlay:

   ```sh
   compose() {
     docker compose --env-file .env.production -p tikkit \
       -f docker-compose.prod.example.yml -f docker-compose.backup.example.yml "$@"
   }
   compose up -d --build backup
   compose logs backup        # first backup runs at once: "backup … complete"
   ```

4. Copy backups off the host after every run with
   [`ops/backup/offsite.sh`](../backup-restore.md#off-host-copy), from cron or a
   systemd timer every few minutes, with a push command of your choice
   (`rclone`, `rsync`, `cp`). The copy is already encrypted.

5. Run the restore drill once on a disposable project, and after each upgrade
   ([restore drill](../backup-restore.md#restore-drill)).

A restored copy only protects you if you also have the backup key and
`ENCRYPTION_KEY`, stored apart from the host and from each other.

## 9. Upgrade

Upgrading needs a maintenance window on the single host.

1. Read the release notes. Note the current image references and keep a verified
   backup from before the upgrade.
2. Set the new `TIKKIT_WEB_IMAGE` and `TIKKIT_API_IMAGE` in `.env.production`.
   With registry images, pull them: `compose pull api web`. With locally built
   images **do not pull; rebuild instead**: check out the new commit, build both
   images under *new* tags (for example `tikkit-api:2026-11-19`; keep the old tags
   for a rollback) and put those tags in `.env.production`.
3. Stop traffic and writers, then take a final on-demand backup and copy it off
   the host:

   ```sh
   compose stop proxy web api
   compose exec backup /opt/tikkit-backup/entrypoint.sh run
   ```

4. Migrate, start and verify:

   ```sh
   compose run --rm --no-deps api migrate
   compose up -d --wait
   curl -fsS https://<PUBLIC_HOST>/api/ready
   ```

   Sign in, open an existing ticket and a screenshot.
5. Keep the previous images and the backup until you are satisfied. Never
   remove volumes while upgrading.

If you are upgrading from the older proxy configuration: remove
`TRUST_PROXY_HEADERS` from your environment (the API refuses to start while it
is enabled) and set `TRUSTED_PROXIES` to the subnet shared by the proxy and the
web server. The reference file already does. See
[troubleshooting](troubleshooting.md#the-api-refuses-to-start-after-an-upgrade-trust_proxy_headers).

A PostgreSQL major-version upgrade is not an image bump; it needs a separate
dump and restore procedure.

## Rollback limits

Migrations are forward-only and checked by version and checksum. An older API
refuses to start, and readiness reports `incompatible`, against a database that
a newer release has migrated.

- **Nothing was migrated, or the migration set is compatible.** Stop the app
  services, restore the previous image references and configuration, run the
  previous image's `api migrate` as a check, start and probe it.
- **Migrations advanced or data changed.** Restore the pre-upgrade backup
  (database and screenshots together) into fresh volumes with the previous
  versions and the same secrets, then switch traffic. You lose everything
  written after that backup. Keep the newer volumes for investigation.
- Never edit migration history to force a rollback.

Details: [deployment](../deployment.md#upgrade-and-rollback). Symptom and fix:
[troubleshooting](troubleshooting.md#readiness-reports-migrations-incompatible-after-a-rollback).

## Restore

A restore always goes into a new Compose project with fresh volumes and starts
in restore mode: it sends no email and calls no AI provider until you finish
the checklist and leave restore mode.

1. Prepare `.env.restore` from your production file: keep the **same**
   `ENCRYPTION_KEY` and the image references recorded in the backup, generate
   new database passwords and `SESSION_SECRET`, and point the proxy at free
   loopback ports with a local certificate:

   ```sh
   (umask 077; cp .env.production .env.restore)
   for name in POSTGRES_PASSWORD TIKKIT_DB_PASSWORD; do
     sed -i.bak "s|^$name=.*|$name=$(openssl rand -hex 32)|" .env.restore
   done
   sed -i.bak "s|^SESSION_SECRET=.*|SESSION_SECRET=$(openssl rand -base64 32)|" .env.restore
   sed -i.bak -E '/^(PUBLIC_HOST|PROXY_BIND|HTTP_PORT|HTTPS_PORT|CADDY_GLOBAL_OPTIONS|BACKUP_)/d' .env.restore
   rm .env.restore.bak
   cat >> .env.restore <<'EOF'
   PROXY_BIND=127.0.0.1
   HTTP_PORT=<free port>
   HTTPS_PORT=<free port>
   PUBLIC_HOST=localhost:<the HTTPS_PORT above>
   CADDY_GLOBAL_OPTIONS=local_certs
   EOF
   ```

   If you built the backup image under another name, pass it with `--image`
   and set `TIKKIT_BACKUP_IMAGE`.
2. Run, from the deployment directory of the machine doing the restore (it needs
   Docker, `jq`, and the `tikkit-backup` image from
   [step 8](#8-set-up-backups-before-real-tickets)):

   ```sh
   ops/backup/restore.sh \
     --backup <path to the off-host copy>/<timestamp> \
     --identity <path to the backup private key> \
     --project tikkit-restore \
     --env-file .env.restore
   ```

3. Work through the [post-restore checklist](../backup-restore.md#post-restore-checklist-before-any-live-work),
   then leave restore mode on purpose by recreating the stack without the
   restore overlay.

To recover production onto a new host use `--project tikkit` there. **Before you
switch from `.env.restore` to `.env.production`**, the restored PostgreSQL keeps the
passwords it was initialized with, so either copy `POSTGRES_PASSWORD` and
`TIKKIT_DB_PASSWORD` from `.env.restore` into `.env.production`, or rotate the
database roles while the restored copy still runs. Otherwise the API cannot connect
([going live after a recovery](../backup-restore.md#going-live-after-a-recovery)).
Recovery point and time objectives, the email cutover and key custody are in
[backup and restore](../backup-restore.md).

## Where next

- Configure the organization: [administrator guide](admin.md).
- Add AI assistance later: [AI guide](ai.md).
- Something does not work: [troubleshooting](troubleshooting.md).
