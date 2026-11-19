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
| The non-secret deployment configuration: the Compose files, `Caddyfile`, `init-postgres.sql`, and `deployment.env` with `PUBLIC_HOST`, the two image references and the port/bind settings | Caddy's TLS state (certificates are issued again), Prometheus data, container logs |
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
   # The private key must never exist with default permissions, even briefly.
   (umask 077; docker run --rm tikkit-backup:local age-keygen > tikkit-backup-key.txt)
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

   A fresh sidecar has no backup since the latest due time, so it runs one
   immediately: the first copy exists within a minute and you see
   `backup … complete` in the log. From then on it follows the
   [schedule rule](#schedule-retention-and-rotation).

The sidecar is deliberately small: a read-only root filesystem, no Linux
capabilities, UID 10001 (the API's UID, which owns the private screenshot
volume), the screenshot volume mounted **read-only**, only the internal `db`
network (it cannot reach the internet and is not published), and the
application database login (`tikkit`), which can read but whose dump needs no
superuser. It holds public keys only, so it cannot decrypt anything it wrote.

### Permissions

Backup directories are created `0700` and every file `0600` (`umask 077`),
owned by UID 10001. Each run writes into its own private staging directory
(`.tmp-<time>.<random>`) and publishes it with one rename to a name that is
checked to be free, so a listing never shows a half-written backup, a crashed
run leaves nothing that looks valid, and a run can never delete or nest inside
another's directory. Runs are serialized by a kernel file lock on a file
descriptor, which is released when the process exits however it exits (a full
disk or a kill cannot leave it stuck). A directory only counts as a backup when
`SHA256SUMS` lists exactly its four encrypted files and they match. Keep
`BACKUP_VOLUME` and any off-host destination restricted to the operators who may
read personal data: encryption protects against disclosure of a stolen copy, not
against a careless mount.

## Schedule, retention and rotation

One rule drives the sidecar: **a backup must have started since the latest due
time** in `BACKUP_TIMES_UTC`.

| Event | Behaviour |
|---|---|
| Scheduled run | Each time in `BACKUP_TIMES_UTC`. Online: users keep working, nothing is stopped. |
| Sidecar down or restarted late | It checks the rule every 20 seconds, so if it comes back at 03:20 after missing 03:15 it runs at once. It does not wait for the next day's slot. A first start has no backup, so it runs at once. |
| Failed run | Logged as `level=error`, recorded for the metrics, the previous good copy stays; retried after 30 minutes (`BACKUP_RETRY_MINUTES`) until it succeeds. |
| Overlapping runs | The second one is refused with exit 75 and touches nothing of the first. |
| Rotation | Only after a **complete, non-degraded** run: copies older than `BACKUP_RETENTION_DAYS` are removed unless they are among the newest `BACKUP_MIN_KEEP` copies that are verified complete (directory not partial, checksums match, not [degraded](#consistency-between-the-database-and-the-screenshots)). A failed or degraded run never rotates, and incomplete or degraded directories never count toward the minimum, so they cannot push the last good copies out. |

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
the dump already had its file when the snapshot was taken: uploads that arrive
during the backup only produce extra files, which are harmless.

The backup keeps **one database session open for its whole duration**
(`PGAPPNAME=tikkit-backup`). In that session it, in order: takes the
screenshot-files advisory lock, starts a `REPEATABLE READ` transaction and
exports its snapshot. Then everything that describes the database comes from that
same snapshot: `pg_dump --snapshot=…`, the list of screenshot ids, the ticket
and screenshot counts and the migration version in the manifest. After the
screenshot copy it ends the transaction and releases the lock **through the same
session**.

- **The lock.** `SELECT pg_advisory_lock(screenshot_files_lock_key())` before
  the dump, `SELECT pg_advisory_unlock(screenshot_files_lock_key())` after the
  screenshot copy. Background work that deletes screenshot files (data
  retention and the stray-file sweep, #43) defers those deletions while it is
  held, so no file the dump references can vanish before it is archived (the
  [contract](retention.md#backups) is defined by migration `0028`, which creates
  the function: use the function, never its number). A database that predates the
  migration has no function; the backup detects that (`to_regprocedure`), logs
  `level=warn … running WITHOUT the screenshot-files lock`, records
  `screenshot_lock_held=no` in the manifest, and takes the backup without it.
- **Lock loss is a failed backup.** The unlock must return `t` from the original
  session. If the backend was terminated, restarted or reconnected, the result
  is not `t` (or the session is gone) and the backup fails: nothing is published
  and nothing is rotated. A merely idle client process is not evidence.
- **Degraded.** If the snapshot lists a screenshot row whose file is not in the
  archive, the backup is still published (it is the best copy there is), marked
  with a plain `DEGRADED` file, logs a warning, sets
  `tikkit_backup_screenshots_missing`, never rotates anything away, and does not
  count toward the retention minimum.

| Situation | What it means | What happens |
|---|---|---|
| A file with no row in the restored database | An upload arrived after the dump (or an abandoned upload). Harmless. | Restore reports the count and the ids. The stray-file sweep (hourly, and at every API start) removes such files once they are older than 24 hours and the API is out of restore mode; the lock defers it while a backup runs. |
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
| `POSTGRES_PASSWORD`, `TIKKIT_DB_PASSWORD` | Database logins | Secret manager | Not needed from the old system. The dump holds no password; a fresh stack creates the roles from the values it is **initialized** with. Those become the restored database's passwords, so carry them into the production env file or [rotate the roles](#going-live-after-a-recovery) before switching env files. |
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
  - The stored SMTP **username and password** (both are encrypted) cannot be
    decrypted. Email is not sent (the delivery error code is
    `smtp_credential_unreadable`); in-app notifications are unaffected. The Admin
    must enter **both the username and the password** again: a password alone
    leaves the stored username unreadable (the drill asserts this).
  - `restore.sh` prints this state at restore time, and
    `api credentials-check` reads it back at any time without sending anything:
    `ai_credentials=readable|unreadable|none smtp_credentials=readable|unreadable|none`.
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
backup container). Instead, [offsite.sh](../ops/backup/offsite.sh) is an
idempotent **reconciliation** you run from cron or a systemd timer every few
minutes (not once a day a few minutes after the backup): it copies every
published backup that has not been shipped yet, newest first, checks each by
checksum without any key, and runs your push command. A run that failed, or one
that raced a failing backup, is simply picked up on the next run, so the
destination never silently lags a day behind.

```sh
ops/backup/offsite.sh --project tikkit --stage /srv/offsite-staging \
  --push 'rclone copy "$BACKUP_DIR" "remote:tikkit-backups/$BACKUP_NAME"' \
  --max-age-hours 30
# or: --push 'rsync -a "$BACKUP_DIR" backup-host:/srv/tikkit-backups/'
```

Your command receives `BACKUP_DIR` (the staged, verified copy) and
`BACKUP_NAME` in its environment. The script prints one status line, for example
`offsite_newest_backup=… offsite_newest_shipped=… offsite_age_seconds=…`, and
exits non-zero when the newest backup is not at the destination or the newest
shipped copy is older than `--max-age-hours`; wire that exit status to cron mail
or your monitoring. That age is the **freshness at the destination**: the data
you could recover if this host were lost right now.

If you set `BACKUP_VOLUME` to a host path on a separate disk or mount you can
sync that directory instead, but keep the same two habits: only ship
directories that verify, and watch the age of the newest one at the destination.

Verify a copy **at the destination** (not just where it was staged) at least
monthly, with the key-free check, which requires the four expected encrypted
files and exactly one checksum entry for each (a copy that lost its manifest and
its checksum line is rejected):

```sh
docker run --rm --user "$(id -u):$(id -g)" -v /path/at/destination/20261118T031500Z:/backup:ro \
  tikkit-backup:local verify /backup
```

Restore from the *off-host* copy, not the local one, when you run the drill by
hand.

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
| `TikkitRestoreModeLeftOn` | `tikkit_restore_mode` is 1 for 12 hours (in the main [alert rules](../ops/prometheus/alerts.yml)) | A restored copy was promoted without leaving [restore mode](#isolated-restore): finish the checklist and recreate the stack without the overlay |

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
  resolution-confirmation timeouts, no AI usage retention, no data retention
  worker (neither the daily automatic run nor the audit purge) and no screenshot
  sweeps. It also refuses to start destructive jobs: `POST
  /api/admin/retention/runs` and account anonymization answer `409
  restore_mode_blocked` with a localized message, because a copy under
  verification must not change (previews, reports and exports still work). It
  sends no invitation or recovery email (the Admin still gets the link to
  copy), and it refuses the SMTP and AI connection tests with
  `409 outbound_disabled`; password recovery answers the same refusal for every
  username and creates no token. It logs `RESTORE_MODE is on` at start
  ([configuration](configuration.md)). Pending outbox rows, in-flight AI attempts
  and not-yet-due deadlines in the restored data therefore stay exactly as
  they were.
- The two protections are independent: removing `RESTORE_MODE` by mistake still
  leaves the container without any route to a mail relay or provider.
- **Its own fixed subnet.** The production Compose file pins the `app` network to
  `172.30.0.0/24` and the API's `TRUSTED_PROXIES` to it ([security](security.md)).
  Two projects cannot share that range, so the restore overlay pins the restored
  project's `app` network, and `TRUSTED_PROXIES` with it, to `RESTORE_APP_SUBNET`
  (default `10.99.48.0/24`). It stays a fixed subnet that nothing else joins; set
  another value in `.env.restore` only if that range is taken on your host.
- **It cannot be left on unnoticed.** Besides the startup warning, `/api/ready`
  reports `"restore_mode": true`, the metrics endpoint exposes
  `tikkit_restore_mode 1` (the `TikkitRestoreModeLeftOn` alert fires after 12
  hours), and every signed-in Admin sees a localized banner.
- **Only images that support it may serve.** An older API image that ignores
  `RESTORE_MODE` would start the outbox, auto-close and sweep workers on the
  restored data. `restore.sh` therefore runs `tikkit-api restore-check` from the
  image before anything is created: it succeeds only when the image knows the
  command **and** `RESTORE_MODE` is on, and `restore.sh` refuses any other image
  ("does not support restore mode"). After start it also requires the running
  API's `/api/ready` to report `restore_mode: true` and stops the services
  otherwise. Use an API image from a release that includes restore mode.

Prepare `.env.restore` next to the Compose files (owner-only). Use the image
references recorded in the backup's `deployment.env` (the API image must embed
the same or a newer migration set than the backup's `migration_version`), the
**same `ENCRYPTION_KEY`** as production, new database passwords and
`SESSION_SECRET`, `PROXY_BIND=127.0.0.1` and free `HTTP_PORT`/`HTTPS_PORT`
(with `PUBLIC_HOST=localhost:<HTTPS_PORT>` and
`CADDY_GLOBAL_OPTIONS=local_certs`; Compose derives the API's `PUBLIC_ORIGIN` as
`https://<PUBLIC_HOST>`, and the proxy serves only that host). Then, from the deployment directory:

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

0. **Claims the project** by creating a reservation network `<project>-reserved`
   (names are unique, so a second run or a leftover fails here), renders the
   Compose config, and refuses if **any** volume, network or container the
   project would use already exists *by exact name*, including a non-empty
   volume without Compose labels. Nothing is changed in that case. It never
   removes anything it did not create.
1. verifies the checksums and decrypts every stream to compare its SHA-256
   with the manifest (fails with a clear message on a damaged copy or a wrong
   key, before creating anything);
2. checks that the API image supports restore mode (see above);
3. starts an empty PostgreSQL and restores the dump in one transaction
   (`pg_restore --exit-on-error --single-transaction`), streaming from age
   without writing plaintext to disk or into the backup directory;
4. restores the screenshot files into the new volume;
5. runs `api migrate` as a compatibility check of this image against the
   restored migration history, prints whether the stored credentials can be
   read with this `ENCRYPTION_KEY` (`api credentials-check`), and applies the
   **email cutover** (below);
6. checks every restored file against the manifest checksums (an empty list is
   fine when the manifest declares zero files; a failing checksum command fails
   the restore), takes an inventory of the regular files by relative path, and
   reports **rows without a file** and **files without a row** explicitly, with
   ids (see [consistency](#consistency-between-the-database-and-the-screenshots));
7. starts the application services in restore mode and checks `/api/ready`.

### The email cutover

A mail relay may have accepted a message *after* the backup snapshot that the
restored database still shows as pending; delivering the restored row would send
it again. The default policy is **suppress everything that was pending at the
backup time**: `restore.sh` runs, inside restore mode,

```sh
tikkit-api suppress-restored-email --before <snapshot time from the manifest>
```

which marks every `pending` **email** row created at or before that time as
`suppressed` (`last_error = suppressed_on_restore`): never sent, kept for history,
counted. In-portal notifications (the inbox) are untouched and still created
normally. It writes one audit event, `notifications_suppressed_on_restore`, with
the cutoff and the counts only (no content), prints the counts, and refuses to
run unless `RESTORE_MODE` is on, so it cannot be used on a live system. Email
rows created after the snapshot (by use of the restored copy) stay pending. The
cost of the policy is the opposite risk: an event whose email was **not**
delivered before the loss is not mailed after the recovery either, and staff
should be told to check the tickets of the last backup interval. Opt out with
`--keep-pending-email` if you prefer duplicates over omissions.

It prints the restore point, the counts, durations, and the single line
`restore_seconds=… screenshot_rows_without_file=… screenshot_files_without_row=… email_suppressed=…`. Open
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
`PUBLIC_HOST`. This deliberately has to be a separate, explicit step.

#### Going live after a recovery

The restored PostgreSQL was initialized with the passwords in the env file you
restored with (`.env.restore`); PostgreSQL ignores a different password in
`.env.production` afterwards and the API would fail to connect. Do one of:

- **Carry them over**: copy `POSTGRES_PASSWORD` and `TIKKIT_DB_PASSWORD` from
  `.env.restore` into `.env.production` (and keep `ENCRYPTION_KEY` the same), or
- **Rotate the roles before switching env files**, with the restored copy still
  running (hex passwords only, as in the [deployment guide](deployment.md)):

  ```sh
  restore() { docker compose --env-file .env.restore -p tikkit \
    -f docker-compose.prod.example.yml -f docker-compose.restore.example.yml "$@"; }
  new_app=$(openssl rand -hex 32); new_admin=$(openssl rand -hex 32)
  printf "ALTER ROLE tikkit PASSWORD '%s';\nALTER ROLE postgres PASSWORD '%s';\n" "$new_app" "$new_admin" \
    | restore exec -T postgres psql -U postgres -d postgres -v ON_ERROR_STOP=1
  # then put $new_app and $new_admin in .env.production as TIKKIT_DB_PASSWORD and POSTGRES_PASSWORD
  ```

Check `docker compose … up -d --wait` reaches healthy before you announce it.

## Post-restore checklist (before any live work)

Do steps 1 to 5 in restore mode, in order. The jobs do not run until step 6.

1. **Verify consistency.** `restore.sh` finished without errors. Sign in as an
   Admin, open the most recent tickets, a closed one, an AI-solved one (visible
   to the Admin only, read-only) and a ticket with screenshots, and compare with
   what you expect at the backup's time (`created_utc` in the manifest). Every
   ticket, comment and audit entry after that time is gone: that is the
   [RPO](#recovery-objectives). Decide how to tell requesters who submitted
   during the lost window.
2. **Credentials.** If `ENCRYPTION_KEY` differs from the old one, re-enter the AI
   key and the SMTP password in Admin settings and re-run the tests after leaving
   restore mode (the tests are refused inside it). `api credentials-check`
   reads the stored credentials back without sending anything.
3. **Duplicate notifications.** The cutover already suppressed the email rows
   that were pending at the backup time (see
   [the email cutover](#the-email-cutover)); read its printed counts and the
   `notifications_suppressed_on_restore` audit event, and tell staff to check
   the tickets of the last backup interval because an event whose email was
   never delivered is not mailed now. The in-app inbox cannot duplicate (one
   inbox row per outbox row). If you chose `--keep-pending-email`, rows still
   pending are delivered once when jobs start, so recipients may get a repeat.
   The retry schedule is capped at 8 attempts; use the Admin retry action for
   rows that reached `failed`.
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
6. **Leave restore mode, still closed to users.** Retention runs and
   anonymization are destructive jobs, so a copy in restore mode refuses them
   (`409 restore_mode_blocked`, localized) and its retention worker does not run:
   the copy is under verification and nothing may change it. When the checks above
   are done, recreate the stack without the restore overlay (see
   [going live](#going-live-after-a-recovery)) but keep the proxy on loopback
   (`PROXY_BIND=127.0.0.1`) and DNS off the host, so only you can reach it. The
   workers now start: outbox rows after the cutoff are delivered, AI attempts and
   confirmation timeouts are handled (steps 4 and 5).
7. **Redo what was removed since the backup, before users arrive.** A restore
   brings back everything [retention](retention.md#backups) had removed and
   everything anonymized after the backup was taken. As an Admin on the closed
   copy:
   - **Retention:** the policy is part of the restored database (check
     `/admin/retention`; if it was changed after the backup, set it again). Preview
     and **start a run** now, and wait until it completes (or, with "apply
     automatically" on, the worker does it within a day). It removes expired
     drafts, screenshots, AI content, ticket content and audit events again
     ([how a run works](retention.md#preview-and-runs)). Audit events are only
     removed once they are past the period you set and the 30-day floor, so a
     restored audit log may keep entries the live system had already purged until
     the run does it. A file deferred by the lock is swept within the hour.
   - **Anonymizations are not replayed.** Anonymize again every account that was
     anonymized after the backup, from the record you keep outside the database
     (the `account_anonymized` audit events list them; note the IDs before a
     restore loses them). Accounts anonymized before the backup stay anonymized:
     the drill checks it.
   - Details and the reasoning are on the [retention page](retention.md#backups),
     which says the same from the other side.
8. **Open to users and take a fresh backup.** Point DNS and the public bind at the
   host, watch the logs for worker start-up, the Admin Email page for outbox
   counts, and the [operations runbook](operations.md) metrics for falling queue
   lag. Take a fresh backup straight away and confirm the first scheduled run.

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
projects afterwards (`DRILL_KEEP=1` keeps them).

**It can only remove what it created.** The two project names default to
`tikkit-drill` and `tikkit-drill-restore`, must start with `tikkit-drill` or
`tikkit-ci-drill` (other names need `--i-know-these-projects-are-disposable`),
must differ, and must own **no** container, volume or network by exact name or
label; otherwise the drill aborts before building, starting or removing anything,
and says what exists. Compose interpolation is isolated from the surrounding
environment (an exported `HTTP_PORT`, `PUBLIC_HOST` or `COMPOSE_FILE` cannot
leak into it), so it is safe next to another stack on the same host or in CI.

What it does and checks:

1. Starts a source stack with the backup sidecar and confirms the sidecar's own
   first backup (also the "zero screenshots" restore case), its metrics, and the
   safety refusals above.
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
3. Takes a backup and checks: permissions (`0700`/`0600`); retention, including
   that bogus directories cannot crowd out the newest complete copies and that a
   degraded run rotates nothing; two simultaneous runs (exactly one is refused
   with 75) and back-to-back runs (distinct, never nested); that the encrypted
   files contain no plaintext secret; the **lock**: with the function hidden (an
   older database) the backup warns and runs; with the real
   `screenshot_files_lock_key()` of migration `0028` the backup session holds
   the advisory lock **whose key is that function's value** while `pg_dump` runs,
   a backup whose session is terminated mid-run **fails** and publishes nothing,
   and a file deletion through the product's own path (anonymizing an account
   with purge of its final tickets, which goes through the retention routine and
   `screenshots::remove_files`) is **deferred while the backup holds the lock**
   (row gone, file still on the volume, backup not degraded, file in the
   archive) and **happens after the release** (the stray-file sweep at API
   start); the scheduler catching up
   with a due time 10 minutes ago after a backup 20 hours ago; and
   `offsite.sh`, which ships the copy, is idempotent and reports freshness. The
   copy is verified by checksum **without the key**, and a damaged copy, a copy
   that lost its manifest and checksum line, and a copy with duplicate checksum
   entries are all rejected.
4. Restore refusals: a project whose volume already exists by name (without
   Compose labels) is refused and the volume left untouched, and an API image
   that cannot confirm restore mode is refused before anything exists.
5. Restores into the isolated project and checks, against the source: ticket
   detail and activity (requester and Admin views) and the operator queue lists
   and counts are identical; the **AI-solved ticket** is read-only for the
   requester, refused for staff, and absent from **each** operator queue;
   report and aggregate statistics (ticket status/resolution/closure counts,
   event counts, AI attempts and usage, account roles, the audit log by action,
   the retention settings and the Admin report totals) are equal, and the
   account anonymized before the backup is still anonymized; every
   screenshot matches by SHA-256 on the volume **and** through the API
   download; the **email cutover** suppressed exactly the email rows that were
   pending, left the in-portal rows pending, and left one audit event; after 25
   seconds nothing moved (the in-flight AI attempt is untouched and no usage was
   recorded); password recovery is refused the same for a known and an unknown
   user and creates no token; a retention run and an anonymization are refused
   (`restore_mode_blocked`) and change nothing; `/api/ready`, the metric and the Admin banner show
   restore mode and the source shows none; the work created after the backup is
   absent (the RPO); the restored API has no egress network; and the backup
   directory was not written to.
6. Restores the backup with **zero screenshots** (a valid installation that never
   had one).
7. Failure simulations: a **missing backup key** (restore stops, creates
   nothing), a **missing `ENCRYPTION_KEY`** (the API refuses to start and names
   the variable), a **wrong `ENCRYPTION_KEY`** (tickets and screenshots stay
   usable, `credentials-check` and the Admin API report both credentials
   unreadable, a password-only SMTP save leaves it unreadable, username plus
   password makes both readable again) and a **missing screenshot object**
   replaced by a directory of the same name (the backup warns, is marked
   degraded and sets `tikkit_backup_screenshots_missing`; restore refuses until
   `--allow-missing-files`, reports exactly that id, and only that screenshot
   returns `500`).

`DRILL_MODE=reduced` skips the two restores of the failure simulations, which
is what CI runs (the reduced-drill step of the `compose` job in the
[release pipeline](ci.md)). `DRILL_BULK_MB=1024` adds about that much data to
the database and the volume so the durations can be extrapolated.
`DRILL_OLD_API_IMAGE` additionally checks that a real older API image is refused.

### Latest recorded run

Run on 2026-11-19 on a developer workstation (Docker Desktop, Apple silicon, API/web images
built from this branch), `ops/backup/drill.sh` in full mode: **88 checks passed**
in 217 seconds, including every failure simulation, with the surrounding shell
deliberately exporting conflicting `HTTP_PORT`, `HTTPS_PORT`, `PUBLIC_HOST` and
`COMPOSE_FILE`. Durations of the mechanical steps:

| Step | Seed data (a few tickets, 4 screenshots) | `DRILL_BULK_MB=1024` (1.24 GB database, 1.07 GB volume) |
|---|---|---|
| Backup run (snapshot, dump, encrypt, archive, checksums) | about 1 s, 156 KB | 54 s, 1.69 GB encrypted (random data does not compress) |
| Restore, total (`restore.sh`, including the cutover) | 27 s | 73 s |
| of which database restore | under 1 s | 13 s |
| of which screenshot restore | under 1 s | 21 s |
| Stack start, health waits, verification | about 25 s | about 39 s |

These are measurements of one machine, not guarantees. The rest of the drill's
time is seeding, the 25-second restore-mode idle check and the failure
simulations.

## Recovery objectives

**Recovery point objective (RPO)** is how much recent work a restore can lose.
The scheduler guarantees that a backup **starts** at every due time, and
immediately when the sidecar was down at that time (it runs the missed slot as
soon as it is back), and retries a failed run every 30 minutes. So the age of
the newest local backup is at most one interval plus the run duration plus, after
a failure, the retry delay:

| Schedule | RPO against loss of the data (worst case, healthy sidecar) |
|---|---|
| Daily (default, `03:15`) | about 24 hours + backup duration; +30 minutes per failed attempt |
| Every 6 hours (`BACKUP_TIMES_UTC=03:15,09:15,15:15,21:15`) | about 6 hours + backup duration; +30 minutes per failed attempt |

Against **loss of the host**, the RPO is that figure plus the lag of the
off-host copy: with `offsite.sh` running every 5 to 15 minutes, add that
interval and the copy time. The age reported by `offsite_age_seconds` is the
actual figure at any moment. If the sidecar itself is down for a long time the
RPO grows with it, which is exactly what `TikkitBackupStale` and the
destination age check are for. Choose the schedule from how much work you can
afford to redo; shorter intervals cost only disk and I/O because backups are
online.

**Recovery time objective (RTO)** is how long until the restored service takes
traffic. It is the sum of: fetching the backup, verification and decryption,
database restore, screenshot restore, container start-up and health checks,
the post-restore checklist, and DNS/TLS. The drill measures the mechanical
part (`restore_total`, with `database_restore_seconds` and
`screenshots_restore_seconds` separately), which grows with data size:

| Data (database + volume) | Backup run | Mechanical restore (`restore_total`) |
|---|---|---|
| a few MB | about 1 s | about 27 s |
| 1.24 GB + 1.07 GB (measured) | 54 s | 73 s |

Beyond a fixed start-up of about 25 to 30 seconds, restore cost grew by roughly
15 to 20 seconds per GB of data on the test machine, and backup cost by about
23 seconds per GB. A single-host deployment with tens of GB therefore restores
mechanically in minutes.

**The measured values above are mechanical restore time only**
(`restore_total`: verification, decryption, database and screenshot restore,
migration check, cutover, start-up and health). They exclude retrieving the
backup from the off-host destination, finding the key, writing `.env.restore`,
the checklist, DNS and TLS, and the cutover to production (switching env files,
leaving restore mode).

Plan the human part separately: finding the key, making `.env.restore`,
reviewing the checklist. A target of **4 hours** for a first recovery is
realistic for this single-host reference deployment when the key and the
off-host copy are at hand, and **dominated by those human steps and by the copy
transfer, not by the restore itself**. Re-measure with `DRILL_BULK_MB` set near
your own database and volume size, and with a real transfer from your off-host
destination, then write your own numbers down.

## Backups and deleted data

Backups are point-in-time copies. Retention, deletion and anonymization in the
application ([retention.md](retention.md#backups) is the same story from the
other side) operate on the live database and **do not rewrite existing backups**:

- **Backup retention bounds real deletion.** A deleted or anonymized person's data
  stays in every backup taken before the deletion until that backup expires
  (`BACKUP_RETENTION_DAYS` locally, plus the retention of your off-host
  destination). Data is gone for good only once every backup that contains it has
  expired: "ticket content 365 days, backups 35 days" means removed content can
  exist for up to 400 days. The backup is encrypted, and restoring it is a
  deliberate, access-controlled act, but the data is there. Keep the off-host
  retention as short as your recovery needs allow.
- **A restore of an older backup brings it all back:** removed drafts,
  screenshots, ticket and AI content, audit events past their period, and the
  identity of an account anonymized after that backup. Redo them before users
  arrive: [checklist step 7](#post-restore-checklist-before-any-live-work) (run
  retention again, anonymize again from the record you keep outside the database).
  Retention is idempotent and its state is in the database, so a policy saved
  before the backup is still in force. Anonymizations are the one thing that is not
  replayed automatically.
- **Audit retention floor.** Audit events cannot be purged younger than 30 days
  ([audit.md](audit.md)), so a backup holds at least the last 30 days of the audit
  trail even where the configured period is shorter; a restore brings back whatever
  the live system had purged since, and the next run purges it again.
- **The backup does not delete anything either.** It holds the screenshot-files
  lock while it runs, which only defers file deletions (retention itself, and
  every other change, proceed) so a backup never contains a row without its file
  because of retention.
- To remove a specific person from backups sooner, delete the affected backup
  directories locally and at the destination (they are plain directories) once
  a newer backup exists, accepting the loss of that restore point.
