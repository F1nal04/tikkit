# Backup, restore and disaster recovery

Tikkit's reference deployment ([deployment guide](deployment.md)) keeps all of
its state in two Docker volumes: `postgres-data` (users, tickets, history,
audit, AI records and statistics, settings, and the AI/SMTP credentials
encrypted with `ENCRYPTION_KEY`) and `screenshots-data` (the private PNG, JPEG
and WebP screenshots). This page covers protecting them: a scheduled, encrypted
backup sidecar, an off-host copy policy, an isolated restore, a repeatable
drill that proves it works, and the recovery objectives that follow from the
measured numbers.

Everything here is optional to run but not optional to decide: an operator who
skips it has chosen "no backups". Nothing needs a paid service. The tooling is
POSIX shell, the PostgreSQL 17 client tools and [age](https://age-encryption.org)
in one small image built from [ops/backup](../ops/backup).

## What is and is not backed up

| In every backup (encrypted) | Not in a backup (keep it elsewhere) |
|---|---|
| A PostgreSQL 17 custom-format dump of the `tikkit` database | `ENCRYPTION_KEY`, `SESSION_SECRET`, `POSTGRES_PASSWORD`, `TIKKIT_DB_PASSWORD` |
| Every screenshot file | The backup decryption key (age identity) |
| The non-secret deployment configuration: the Compose files, `Caddyfile`, `init-postgres.sql`, and `deployment.env` with `PUBLIC_ORIGIN`, the two image references and the port/bind settings | Caddy's TLS state (certificates are issued again), Prometheus data, container logs |
| An encrypted manifest: backup time, migration version, ticket and screenshot counts, SHA-256 of every stream and of every screenshot file | |

`deployment.env` is built from a fixed allowlist of variable names in
[backup.sh](../ops/backup/backup.sh). No other environment variable of the
sidecar is written to a backup, so a secret added to the Compose file later
cannot leak into it.

## Set up scheduled backups

1. **Create the backup key pair on a trusted machine, not on the host.** The
   host only ever gets the public key, so a compromised host can write backups
   but cannot read old ones.

   ```sh
   docker build -f ops/backup/Dockerfile -t tikkit-backup:local .
   docker run --rm tikkit-backup:local age-keygen > tikkit-backup-key.txt
   chmod 600 tikkit-backup-key.txt
   grep 'public key' tikkit-backup-key.txt
   ```

   Move `tikkit-backup-key.txt` into your password manager or offline storage
   (see [key custody](#encryption-and-key-custody)) and delete the local copy.

2. **Configure the sidecar** in `.env.production`. `BACKUP_AGE_RECIPIENTS` is
   the only required value: one or more space-separated `age1…` public keys.
   Listing two (for example two custodians) means either can restore.

   | Variable | Default | Meaning |
   |---|---|---|
   | `BACKUP_AGE_RECIPIENTS` | required | Public age keys the backups are encrypted to. |
   | `BACKUP_TIMES_UTC` | `03:15` | Daily run times, `HH:MM` UTC, comma separated. |
   | `BACKUP_RETENTION_DAYS` | `14` | Local copies older than this are removed after a successful run. |
   | `BACKUP_MIN_KEEP` | `3` | The newest copies are always kept, whatever their age. |
   | `BACKUP_VOLUME` | `backup-data` | Named volume, or an absolute host path owned by UID 10001 with mode `0700` (put it on a different disk). |
   | `TIKKIT_BACKUP_IMAGE` | `tikkit-backup:local` | Image reference if you build and publish the image yourself. |

3. **Start it** with the same project name and env file as the stack:

   ```sh
   docker compose --env-file .env.production -p tikkit \
     -f docker-compose.prod.example.yml -f docker-compose.backup.example.yml \
     up -d --build backup
   docker compose --env-file .env.production -p tikkit \
     -f docker-compose.prod.example.yml -f docker-compose.backup.example.yml \
     logs backup
   ```

   With no backup in the last 26 hours the sidecar runs one immediately, so
   the first copy exists within a minute and you see `backup … complete` in
   the log. From then on it runs at `BACKUP_TIMES_UTC`.

The sidecar is deliberately small: a read-only root filesystem, no Linux
capabilities, UID 10001 (the API's UID, which owns the private screenshot
volume), the screenshot volume mounted **read-only**, only the internal `db`
network (it cannot reach the internet and is not published), and the
application database login (`tikkit`), which can read but whose dump needs no
superuser. It holds public keys only, so it cannot decrypt anything it wrote.

### Permissions

Backup directories are created `0700` and every file `0600` (`umask 077`),
owned by UID 10001. A backup is written to a hidden `.tmp-…` directory and
renamed to its final name only when every file and `SHA256SUMS` exist, so a
listing never shows a half-written backup, and a crashed run leaves nothing
that looks valid. Keep `BACKUP_VOLUME` and any off-host destination restricted
to the operators who may read personal data: encryption protects against
disclosure of a stolen copy, not against a careless mount.

## Schedule, retention and rotation

| Event | Behaviour |
|---|---|
| Scheduled run | Each time in `BACKUP_TIMES_UTC`. Online: users keep working, nothing is stopped. |
| Start of the sidecar | Runs one backup at once if the last success is older than 26 hours (`BACKUP_MAX_AGE_HOURS`). |
| Failed run | Logged as `level=error`, recorded for the metrics, the previous good copy stays; retried after 30 minutes (`BACKUP_RETRY_MINUTES`) until it succeeds. |
| Overlapping runs | A lock directory refuses the second one (exit 75). |
| Rotation | After every **successful** run, copies older than `BACKUP_RETENTION_DAYS` are removed unless they are among the newest `BACKUP_MIN_KEEP`. A failing backup therefore never shrinks your history. |

One local copy on the same disk is not a disaster-recovery plan; that is what
the [off-host copy](#off-host-copy) is for. Size the local volume for
`BACKUP_RETENTION_DAYS` times the encrypted size (shown by
`tikkit_backup_last_size_bytes`).

You can also run a backup on demand, for example before an upgrade:

```sh
docker compose --env-file .env.production -p tikkit \
  -f docker-compose.prod.example.yml -f docker-compose.backup.example.yml \
  exec backup /opt/tikkit-backup/entrypoint.sh run
```

## Consistency between the database and the screenshots

A backup takes the **database dump first, then the screenshot files, then the
configuration**. The dump is one consistent PostgreSQL snapshot and needs no
downtime. Screenshot files are immutable and written by atomic rename (a file
is complete or absent), so copying them second guarantees that every row in
the dump already had its file when the dump started: uploads that arrive
during the backup only produce extra files, which are harmless.

To keep deletions from breaking that guarantee, the backup holds **one
database session open for its whole duration** that takes a session-level
PostgreSQL advisory lock before the dump and releases it after the screenshot
copy (`lock_acquire`/`lock_release` in [backup.sh](../ops/backup/backup.sh)).
Background work that deletes screenshot files (data retention and the
stray-file sweep) defers those deletions while the lock is held, so no file the
dump references can vanish before it is archived. If the holding session dies
mid-run the backup fails instead of pretending it was protected, and a second
backup cannot start while one holds the lock. The lock key in the script is a
**placeholder** until the retention work fixes the real one
(`BACKUP_LOCK_SQL_ARGS` overrides it, `BACKUP_ADVISORY_LOCK=off` skips the
lock); on a deployment without that work nothing waits on it and the backup
behaves the same.

| Situation | What it means | What happens |
|---|---|---|
| A file with no row in the restored database | An upload arrived after the dump (or an abandoned upload). Harmless. | Restore reports the count and the ids. The stray-file sweep removes such files 24 hours after the API leaves restore mode. |
| A row whose file is missing from the archive | A pending upload expired between the dump and the copy, or, for an attached screenshot, the source volume lost the file. | The backup logs `level=warn` and exposes `tikkit_backup_screenshots_missing`. `restore.sh` **refuses** such a backup unless you pass `--allow-missing-files`. Ticket content is intact; that screenshot's preview and download return `500` and are logged, as for any record whose file is missing ([screenshots](screenshots.md#storage-backup-and-restore)). |
| A volume older than the database | Cannot happen: both come from the same backup directory. Restoring a mismatched pair by hand is what leaves rows whose previews fail. | Always restore with `restore.sh`. |

Attached screenshots are never deleted by the application, so a non-zero
`tikkit_backup_screenshots_missing` always means the live volume has lost a
file. Investigate the host before it happens again; restoring an older backup
that still has the file is the repair.

## Encryption and key custody

Backups are encrypted with age to the public keys in `BACKUP_AGE_RECIPIENTS`
(X25519 key agreement and authenticated encryption per 64 KiB chunk, so tampering and
truncation are detected on decryption). Each backup directory also has a plain
`SHA256SUMS` of the encrypted files, so an off-host copy can be checked for
damage by anyone, without the key.

Four different secrets matter, and **none of them is stored with the backups**:

| Secret | Protects | Keep it | If it is missing at restore |
|---|---|---|---|
| **Backup decryption key** (age identity) | The backups themselves | Offline or in a password manager, at least two custodians, never on the application host or next to the backups | The backup cannot be read; nothing can recover it. `restore.sh` stops in step 1 with `cannot decrypt the manifest: wrong or missing backup decryption key`, before it starts or creates anything. Plan for this: keep two recipients, and test the key (the drill does). |
| **`ENCRYPTION_KEY`** | The AI provider key(s) and the SMTP password stored in the database | In your secret manager, separate from the backups and from the backup key | See below. |
| `POSTGRES_PASSWORD`, `TIKKIT_DB_PASSWORD` | Database logins | Secret manager | Not needed from the old system. The dump holds no password; a fresh stack creates the roles from the new values. |
| `SESSION_SECRET` | Must be set; nothing stored in the database depends on its value | Secret manager | Generate a new one; users sign in again. |

### When `ENCRYPTION_KEY` is missing or wrong after a restore

Behaviour verified by the [drill](#restore-drill) and the integration tests:

- **Missing** (unset or empty): the API refuses to start and logs
  `ENCRYPTION_KEY must be exactly 32 random bytes …`; the Compose file also
  refuses to render without it. There is no unencrypted fallback. Generate a
  new key and start again; this is now the "wrong key" case.
- **Wrong** (a different valid key): the stack starts and **ticketing is fully
  usable**: sign-in, tickets, history, screenshots, queues and reports are
  unaffected, because none of that is encrypted with this key. Only stored
  credentials are lost:
  - The Admin AI settings show the stored provider key as unreadable and
    status `credential_unreadable`. AI is treated as unavailable, so new
    tickets go straight to the shared queue (nothing waits for AI) and the
    connection test answers `ai_credential_unreadable`. The Admin enters the
    provider key again.
  - The stored SMTP password cannot be decrypted. Email is not sent (the
    delivery error code is `smtp_credential_unreadable`); in-app notifications
    are unaffected. The Admin enters the SMTP password again.
  - Nothing else needs repair and no data is lost. Re-entered credentials are
    encrypted with the new key.

The key is not recoverable from a backup, by design. Keep it where the
deployment's other secrets live, and back that up on its own schedule.

### Rotating the backup key

Add the new public key to `BACKUP_AGE_RECIPIENTS` (old and new together), then
recreate the sidecar. New backups are readable with either key. Keep the old
identity until the last backup encrypted only to it has expired, then remove
the old public key from the list.

## Off-host copy

Policy: **after every backup, at least one encrypted copy is stored somewhere
that survives the loss of this host and its disks** (another machine, an
object store, a different site). The copy is already encrypted, so the
destination needs no key and no trust beyond availability. The destination
keeps its own retention, at least as long as the local one.

The sidecar does not push anywhere itself (that would need credentials on the
backup container). Run one of these from the host, from cron or a systemd
timer a few minutes after the backup time:

```sh
compose() { docker compose --env-file .env.production -p tikkit \
  -f docker-compose.prod.example.yml -f docker-compose.backup.example.yml "$@"; }
latest=$(compose exec -T backup sh -c 'ls /backups | grep "^2" | sort | tail -n 1' | tr -d '\r')
mkdir -p /srv/offsite-staging && chmod 700 /srv/offsite-staging
compose cp "backup:/backups/$latest" "/srv/offsite-staging/$latest"
# Detect damage in the copy without the key:
docker run --rm -v "/srv/offsite-staging/$latest:/backup:ro" tikkit-backup:local verify /backup
# Then ship it with your tool of choice, for example:
rclone copy "/srv/offsite-staging/$latest" "remote:tikkit-backups/$latest"
# or: rsync -a "/srv/offsite-staging/$latest" backup-host:/srv/tikkit-backups/
```

If you set `BACKUP_VOLUME` to a host path on a separate disk or mount, you can
sync that directory directly instead. Verify the destination copy the same way
(`verify` with the directory mounted) at least monthly, and restore from the
*off-host* copy, not the local one, when you run the drill by hand.

## Failure alerts

Every failure has three signals, so it is visible with or without Prometheus:

1. The run exits non-zero and logs a line with `level=error`:
   `docker compose … logs backup | grep level=error`. The scheduler retries
   after 30 minutes.
2. The sidecar's Compose health check (`docker compose ps`) shows `unhealthy`
   when its status endpoint stops answering.
3. It serves Prometheus text metrics on port 9092 of the private `db` network
   (not published):

   | Metric | Meaning |
   |---|---|
   | `tikkit_backup_last_run_success` | `1` after a good run, `0` after a failed one |
   | `tikkit_backup_last_success_timestamp_seconds` | when the last good run started (`0`: never) |
   | `tikkit_backup_last_attempt_timestamp_seconds` | when the last run of any outcome started |
   | `tikkit_backup_last_duration_seconds`, `tikkit_backup_last_size_bytes` | duration and encrypted size, for [RPO/RTO](#recovery-objectives) tracking |
   | `tikkit_backup_screenshots_missing` | screenshot rows without a file at the last backup |
   | `tikkit_backup_copies` | copies in the local directory |

To alert with the optional Prometheus from [operations](operations.md), add
the two overlays to the same project and recreate Prometheus:

```sh
docker compose --env-file .env.production -p tikkit \
  -f docker-compose.prod.example.yml -f docker-compose.backup.example.yml \
  -f docker-compose.observability.example.yml -f docker-compose.backup-alerts.example.yml up -d
```

The [alert rules](../ops/prometheus/backup-alerts.yml) (tested by
[backup-alerts.test.yml](../ops/prometheus/backup-alerts.test.yml) in CI):

| Alert | Fires when | First check |
|---|---|---|
| `TikkitBackupFailed` | last run failed for 5 minutes | `logs backup`; usual causes: full disk, database unreachable, the recipient key list edited wrongly, or a table created by hand under another database role that the `tikkit` login cannot read (`pg_dump: permission denied`) |
| `TikkitBackupStale` | no success for 30 hours | The sidecar is not running or keeps failing. 30 hours suits the default daily schedule; use 1.25 times your interval |
| `TikkitBackupMissingScreenshots` | any screenshot row has no file | See [consistency](#consistency-between-the-database-and-the-screenshots) |
| `TikkitBackupSidecarDown` | the status endpoint cannot be scraped for 10 minutes | `compose ps backup`, restart it |

Without Prometheus, a cron job on the host can fetch the same metrics and
mail the operator: `compose exec -T backup wget -qO- http://127.0.0.1:9092/metrics`
and compare `tikkit_backup_last_success_timestamp_seconds` to `date +%s`.

## Isolated restore

A restore always goes into a **new Compose project with fresh volumes** and
starts in **restore mode**, so a copy brought up for verification can never
send a notification or call an AI provider:

- `docker-compose.restore.example.yml` sets `RESTORE_MODE=true`, removes the
  API's `egress` network (the container is attached to the internal `app` and
  `db` networks only) and publishes the proxy on `127.0.0.1` only.
- In restore mode the API still serves requests, but starts **no background
  job**: no notification outbox delivery, no AI attempt or sweep processing, no
  resolution-confirmation timeouts, no usage retention or screenshot sweeps. It
  sends no invitation or recovery email (the Admin still gets the link to
  copy), and it refuses the SMTP and AI connection tests with
  `409 outbound_disabled`. It logs `RESTORE_MODE is on` at start
  ([configuration](configuration.md)). Pending outbox rows, in-flight AI attempts
  and not-yet-due deadlines in the restored data therefore stay exactly as
  they were.
- The two protections are independent: removing `RESTORE_MODE` by mistake still
  leaves the container without any route to a mail relay or provider.

Prepare `.env.restore` next to the Compose files (owner-only). Use the image
references recorded in the backup's `deployment.env` (the API image must embed
the same or a newer migration set than the backup's `migration_version`), the
**same `ENCRYPTION_KEY`** as production, new database passwords and
`SESSION_SECRET`, `PROXY_BIND=127.0.0.1` and free `HTTP_PORT`/`HTTPS_PORT`
(with `PUBLIC_ORIGIN=https://localhost:<HTTPS_PORT>` and
`CADDY_GLOBAL_OPTIONS=local_certs`). Then, from the deployment directory:

```sh
ops/backup/restore.sh \
  --backup /srv/offsite-staging/20261118T031500Z \
  --identity ~/tikkit-backup-key.txt \
  --project tikkit-restore \
  --env-file .env.restore \
  --image tikkit-backup:local \
  --config-out ./restored-config
```

The script, in order:

1. verifies the checksums and decrypts every stream to compare its SHA-256
   with the manifest (fails with a clear message on a damaged copy or a wrong
   key, before creating anything);
2. refuses to run if the project name already has containers or volumes;
3. starts an empty PostgreSQL and restores the dump in one transaction
   (`pg_restore --exit-on-error --single-transaction`), streaming from age
   without writing plaintext to disk;
4. restores the screenshot files into the new volume;
5. runs `api migrate` as a compatibility check of this image against the
   restored migration history;
6. checks every restored file against the manifest checksums and compares
   rows to files (see [consistency](#consistency-between-the-database-and-the-screenshots));
7. starts the application services in restore mode.

It prints the restore point, the counts, durations, and the single line
`restore_seconds=… screenshot_rows_without_file=…`. Open
`https://localhost:<HTTPS_PORT>` (trust the project's local Caddy CA as in the
[deployment guide](deployment.md)) and work through the checklist below. The
restore-mode copy contains the same personal data as production: protect it the
same way and remove it when done:

```sh
docker compose --env-file .env.restore -p tikkit-restore \
  -f docker-compose.prod.example.yml -f docker-compose.restore.example.yml down --volumes
```

### Recovering production onto a new host

The same script performs a real recovery: run it on the new host with
`--project tikkit` (the production project name, which has no volumes there).
Once the checklist is satisfied, leave restore mode by recreating the stack
**without** the restore overlay and with the production env file:

```sh
docker compose --env-file .env.production -p tikkit -f docker-compose.prod.example.yml up -d
```

Point DNS at the new host first; Caddy issues certificates for the real
`PUBLIC_ORIGIN`. This deliberately has to be a separate, explicit step.

## Post-restore checklist (before any live work)

Do these in restore mode, in order. The jobs do not run until step 6.

1. **Verify consistency.** `restore.sh` finished without errors. Sign in as an
   Admin, open the most recent tickets, a closed one, an AI-solved one (visible
   to the Admin only, read-only) and a ticket with screenshots, and compare with
   what you expect at the backup's time (`created_utc` in the manifest). Every
   ticket, comment and audit entry after that time is gone: that is the
   [RPO](#recovery-objectives). Decide how to tell requesters who submitted
   during the lost window.
2. **Credentials.** If `ENCRYPTION_KEY` differs from the old one, re-enter the AI
   key and the SMTP password in Admin settings and re-run the tests after leaving
   restore mode (the tests are refused inside it).
3. **Duplicate notifications.** Outbox rows that were `pending` at the backup
   may already have been delivered by the old system after it. Rows are
   delivered, once, when jobs start, so those recipients can receive a repeat of
   an email about an event from the last backup interval. The in-app inbox
   cannot duplicate (one inbox row per outbox row). Warn staff, or leave SMTP
   disabled in Admin settings until you have looked at the pending rows on the Admin Email
   page, then enable it. The retry schedule is capped at 8 attempts; use the
   Admin retry action for rows that reached `failed`.
4. **AI attempts in flight.** An attempt still `processing` keeps its original
   deadline. Past the deadline (the usual case, since the deadline is minutes)
   the sweep hands the ticket to the shared queue exactly once, with no provider
   call. An attempt whose deadline has not passed is called once. Either way the
   requester ends with a usable ticket; AI-solved records are final and are
   never touched.
5. **Confirmation deadlines.** Deadlines are absolute timestamps. Staff
   proposals whose deadline passed during the outage close as timed-out on the
   first poll after you leave restore mode, and a requester who could not answer
   during the outage cannot reopen it unless the Admin reopening toggle is on.
   After a long outage consider turning that toggle on for a few days, and
   telling requesters.
6. **Retention and deletion.** Anything deleted or anonymized after the backup
   exists again in the restored copy; follow the
   <!-- TODO(#43): replace with the final link when docs/retention.md lands -->
   [data retention and deletion guide](retention.md) to re-apply
   deletion and anonymization requests made since the backup **before** the
   copy serves users. Deletion never rewrites backups; see
   [Backups and deleted data](#backups-and-deleted-data).
7. **Leave restore mode.** For a drill, stop here and remove the project. For a
   real recovery, recreate the stack without the overlay (see above), then watch
   the logs for worker start-up, the Admin Email page for outbox counts, and
   the [operations runbook](operations.md) metrics for falling queue lag.
8. **Take a fresh backup** straight away and confirm the first scheduled run.

## Restore drill

[ops/backup/drill.sh](../ops/backup/drill.sh) repeats the whole procedure on a
disposable pair of Compose projects, so you can run it before trusting the
setup, after each upgrade, and on a schedule:

```sh
DRILL_API_IMAGE=ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z \
DRILL_WEB_IMAGE=ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z \
  ops/backup/drill.sh
```

It needs Docker with Compose v2.33.1+, `jq`, `curl` and `openssl`, uses ports
`DRILL_PORT_BASE` (default 18444) and the next few above it, and removes both
projects afterwards (`DRILL_KEEP=1` keeps them). What it does and checks:

1. Starts a source stack with the backup sidecar and confirms the sidecar's own
   first backup, metrics and schedule catch-up.
2. Seeds mixed outcomes through the real API: an **operator-resolved** ticket
   accepted by the requester, an **auto-closed** one (the snapshotted deadline
   expires and the worker closes it), a **cancelled** one, an **open** one in
   the shared queue, an **AI-solved** one, and an **AI attempt still in
   flight**, each with a screenshot. The one thing a Compose stack cannot do
   offline is the provider call, so the AI step runs against an API in restore
   mode and the provider's answer is written by SQL; submission, the attempt
   row, and the requester's explicit "Solved" decision are the production code
   paths. SMTP and an AI key are stored encrypted but never enabled against a
   real server.
3. Takes a backup, checks permissions (`0700`/`0600`), retention, that the
   encrypted files contain no plaintext secret, copies it off the volume as an
   operator would, verifies the copy by checksum **without the key**, and shows
   that a damaged copy is rejected.
4. Restores it into the isolated project and checks that, against the source:
   ticket detail and activity (requester and Admin views) and the operator
   queue lists and counts are identical; the **AI-solved ticket** is read-only
   for the requester, refused for staff, and absent from the operator queues;
   report and aggregate statistics (ticket status/resolution/closure counts,
   event counts, AI attempts and usage, audit events, account roles) are equal;
   every screenshot matches by SHA-256 on the volume **and** through the API
   download; the notification outbox is carried over unchanged; and after 25
   seconds in restore mode nothing moved (pending rows are still pending, the
   in-flight AI attempt is untouched and no usage was recorded). It also checks
   that work created after the backup is absent (the RPO) and that the restored
   API has no egress network.
5. Failure simulations: a **missing backup key** (restore stops, creates
   nothing), a **missing `ENCRYPTION_KEY`** (the API refuses to start and names
   the variable), a **wrong `ENCRYPTION_KEY`** (tickets and screenshots stay
   usable, the AI credential is reported unreadable and AI unavailable, the
   Admin can store new credentials) and a **missing screenshot object** (the
   backup warns and sets `tikkit_backup_screenshots_missing`; restore refuses
   until `--allow-missing-files`; then only that screenshot returns `500`).

`DRILL_MODE=reduced` skips the two restores of the failure simulations, which
is what CI runs (the `backup-drill` step of the `compose` job in the
[release pipeline](ci.md)). `DRILL_BULK_MB=1024` adds about that much data to
the database and the volume so the durations can be extrapolated.

### Latest recorded run

Run on 2026-11-19 on a developer workstation (Docker Desktop, Apple silicon, API/web images
built from this branch), `ops/backup/drill.sh` in full mode: **37 checks passed**
in 165 seconds, including all four failure simulations. Durations of the
mechanical steps:

| Step | Seed data (a few tickets, 4 screenshots) | `DRILL_BULK_MB=1024` (1.24 GB database, 1.07 GB volume) |
|---|---|---|
| Backup run (dump, encrypt, archive, checksums) | under 1 s, 156 KB | 47 s, 1.69 GB encrypted (random data does not compress) |
| Restore, total (`restore.sh`) | 26 s | 71 s |
| of which database restore | 1 s | 13 s |
| of which screenshot restore | under 1 s | 21 s |
| Stack start, health waits, verification | about 25 s | about 37 s |

These are measurements of one machine, not guarantees. The rest of the drill's
time is seeding, the 25-second restore-mode idle check and the failure
simulations.

## Recovery objectives

**Recovery point objective (RPO)** is how much recent work a restore can lose.
Backups are taken at the scheduled times, so the worst case is one full
interval plus the run time, and a failed run extends it by the retry interval:

| Schedule | RPO (worst case) |
|---|---|
| Daily (default, `03:15`) | about 24 hours + backup duration (a failure at 03:15 retried until success moves the next point to the next run) |
| Every 6 hours (`BACKUP_TIMES_UTC=03:15,09:15,15:15,21:15`) | about 6 hours + backup duration |

Choose the schedule from how much work you can afford to redo; shorter
intervals cost only disk and I/O because backups are online. The objective is
met only if the off-host copy keeps up: until a copy has left the host, the
host's loss loses that backup too.

**Recovery time objective (RTO)** is how long until the restored service takes
traffic. It is the sum of: fetching the backup, verification and decryption,
database restore, screenshot restore, container start-up and health checks,
the post-restore checklist, and DNS/TLS. The drill measures the mechanical
part (`restore_total`, with `database_restore_seconds` and
`screenshots_restore_seconds` separately), which grows with data size:

| Data (database + volume) | Backup run | Mechanical restore (`restore_total`) |
|---|---|---|
| a few MB | under 1 s | about 30 s |
| 1.24 GB + 1.07 GB (measured) | 47 s | 71 s |

Beyond a fixed start-up of about 25 to 30 seconds, restore cost grew by roughly
15 to 20 seconds per GB of data on the test machine, and backup cost by about
20 seconds per GB. A single-host deployment with tens of GB therefore restores
mechanically in minutes.

Plan the human part separately: finding the key, making `.env.restore`,
reviewing the checklist. A target of **4 hours** for a first recovery is
realistic for this single-host reference deployment when the key and the
off-host copy are at hand, and **dominated by those human steps and by the copy
transfer, not by the restore itself**. Re-measure with `DRILL_BULK_MB` set near
your own database and volume size, and with a real transfer from your off-host
destination, then write your own numbers down.

## Backups and deleted data

Backups are point-in-time copies. Retention, deletion and anonymization in the
application (see the <!-- TODO(#43): link --> [retention guide](retention.md))
operate on the live database and **do not rewrite existing backups**:

- A deleted or anonymized person's data stays in every backup taken before the
  deletion until that backup expires (`BACKUP_RETENTION_DAYS` locally, plus the
  retention of your off-host destination). The backup is encrypted, and
  restoring it is a deliberate, access-controlled act, but the data is there.
  Set the off-host retention with this in mind, and keep it as short as your
  recovery needs allow.
- After restoring a backup taken **before** a deletion or anonymization, the
  data is back. Re-apply every deletion request made since the backup before
  letting the copy serve users (checklist step 6). Keep a record of deletion
  requests outside the application database so this is possible.
- To remove a specific person from backups sooner, delete the affected backup
  directories locally and at the destination (they are plain directories) once
  a newer backup exists, accepting the loss of that restore point.
