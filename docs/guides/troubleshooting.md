# Troubleshooting

Symptom, likely cause, fix. These are the problems an operator of a fresh install
actually meets. Each entry links to the detailed runbook section. Commands use the
`compose` helper from the [install guide](install.md#4-start-the-stack-in-order).
First look: `compose ps`, `compose logs --since 30m --tail 300 web api postgres`,
and `curl -fsS https://<PUBLIC_HOST>/api/ready`. Every API response carries an
`x-request-id`; search the logs for it
([operations runbook](../operations.md#api-and-database)).

## Every save is refused with `csrf_rejected`

**Symptom.** Signing in, saving or submitting fails in the portal with "This request
was blocked: open the portal at its configured address (the link in your invitation
or from your administrator) and try again. Requests from other sites are refused."
(HTTP 403, code `csrf_rejected`). Pages load fine. In the API response the same
refusal reads "Open the portal at its configured address (PUBLIC_ORIGIN);
cross-site requests and requests without X-Requested-With: tikkit are refused."
(visible in browser developer tools or in `curl` output).

**Cause.** The API accepts a state-changing browser request only from the exact
origin `https://<PUBLIC_HOST>`. Opening the portal by IP address, a second
hostname, plain `http`, or without the nonstandard port in `PUBLIC_HOST`, or a
`PUBLIC_HOST` that does not match what users type, causes this. Other hostnames get
HTTP 421 or a TLS error from the proxy.

**Fix.**

1. Open the portal at exactly `https://<PUBLIC_HOST>`.
2. If `PUBLIC_HOST` itself is wrong, correct it in `.env.production` (lowercase
   hostname, no scheme or path; include a nonstandard port and set the same
   `HTTPS_PORT`), then run `compose up -d --wait` so the proxy and API are
   recreated. Sign in again; sessions belong to the origin.
3. In a private bootstrap, `PUBLIC_HOST` is `localhost:<port>`: use that, not
   `127.0.0.1`.

Reference: [security](../security.md#csrf-and-origin-policy),
[configuration](../configuration.md). See also
[install](install.md#move-from-the-private-bootstrap-to-the-public-origin).

## The API refuses to start after an upgrade (`TRUST_PROXY_HEADERS`)

**Symptom.** The `api` container exits at once. `compose logs api` shows
"invalid configuration" and "TRUST_PROXY_HEADERS was replaced by TRUSTED_PROXIES".
Compose shows the service restarting.

**Cause.** The old `TRUST_PROXY_HEADERS` switch is still set in your environment or
an override file. The API refuses it because it trusted forwarded addresses from any
peer.

**Fix.** Remove `TRUST_PROXY_HEADERS` everywhere. Make sure `TRUSTED_PROXIES` is the
subnet shared by the proxy and the web server (the reference Compose file sets
`172.30.0.0/24`; a customized network needs its own exact subnet). Set `PUBLIC_HOST`
in `.env.production`, then `compose up -d --wait`. Without `TRUSTED_PROXIES`, every
client behind the proxy shares one rate-limit bucket and audit address.

The same startup output lists every configuration problem at once without printing
secret values (for example "ENCRYPTION_KEY must be exactly 32 random bytes").
Reference: [configuration upgrades](../configuration.md#upgrading),
[operations](../operations.md#upgrading).

## Readiness reports `migrations: incompatible` after a rollback

**Symptom.** `/api/ready` answers HTTP 503 with
`{"status":"unavailable","checks":{"database":"ok","migrations":"incompatible"}}`,
and the API does not start or stays unready after you went back to older images.
(`pending` means migrations still have to be applied; `unknown` means the history
could not be read.)

**Cause.** A newer release already applied migrations the older API does not know.
Migrations are forward-only; an old image cannot run against a newer schema.

**Fix.** Do not edit migration history. Either run the **newer** API image again, or
restore the pre-upgrade backup (database and screenshots together) into fresh volumes
with the older images, accepting the loss of data written after that backup. Keep the
newer volumes for investigation. For `pending`, run
`compose run --rm --no-deps api migrate` with the matching image. Reference:
[install, rollback limits](install.md#rollback-limits),
[deployment](../deployment.md#upgrade-and-rollback).

## Postgres stays unhealthy on the first start

**Symptom.** `compose ps` shows `postgres` unhealthy; `compose logs postgres` shows a
permission error on `10-tikkit.sql`.

**Cause.** The mounted `ops/compose/init-postgres.sql` or `ops/caddy/Caddyfile` is not
world-readable, so the volume was initialized without the `tikkit` login.

**Fix.** `chmod 644` both files. The initializer runs only on an empty volume, so on
a failed fresh install remove the project's volumes and start again; do not do that on
an install with data. Reference:
[deployment](../deployment.md#postgresql-initialized-without-the-application-login).

## SMTP test fails or no email arrives

**Symptom.** **Send test email** fails, or invitations and notifications do not
arrive.

| Message | Cause | Fix |
| --- | --- | --- |
| "The mail server could not be reached" | DNS, firewall or wrong host/port | Allow outbound DNS and TCP to the relay port from the host; check host and port. |
| "Could not establish a verified TLS connection to the mail server" | Wrong transport security for the port, or a certificate that does not match the host | Use STARTTLS on 587 or implicit TLS on 465. The certificate is always verified; use the relay's real hostname. |
| "The mail server rejected the username or password" | Wrong credentials | Enter **both** username and password again. |
| "The mail server refused the message" | The relay refuses the sender or recipient | Use a sender address your relay allows. |
| "The stored mail password could not be read" | `ENCRYPTION_KEY` changed or was lost | [Enter the credentials again](#stored-credentials-are-unreadable-after-a-restore-or-key-change). |
| "This restored copy is in restore mode and sends nothing…" | [Restore mode](#restore-mode-was-left-on) | Leave restore mode. |

Also check: the connection is **Enabled**; the test can only go to your own account
email or the sender address; an email only reaches accounts that still have access to
the ticket; replying to an email never updates a ticket. Failed deliveries retry in the
background and never undo ticket work. After fixing the relay, let pending rows retry;
rows that reached the failed state are re-queued with the Admin API
(`POST /api/admin/notifications/retry`; there is no button for it in the portal). Reference:
[operations, notifications and SMTP](../operations.md#notifications-and-smtp),
[admin guide](admin.md#outbound-email).

## AI assistance is unavailable or tickets skip it

**Symptom.** New tickets go straight to the help desk; the AI page shows a status other
than "enabled and configured", or tickets arrive without a suggestion.

**Causes and fixes.**

- **AI is off or has no key:** enable it and store a key ([ai guide](ai.md#set-it-up)).
- **"stored API key cannot be read":** `ENCRYPTION_KEY` differs; enter the key again.
- **"model … no longer supported":** choose another model in the selector.
- **Test connection fails:** key rejected (replace it), rate limit or quota (check the
  provider's billing), model not available to this key (choose another), or provider
  unreachable (allow outbound DNS and HTTPS from the API host).
- **Budget used up, circuit breaker open, too many calls in progress:** see
  **Usage** and **Budgets and limits** on the AI page. Tickets are never blocked; they go
  to the help desk.
- **The category is set to Skip AI automatically**, or the requester skipped it where
  allowed: working as designed.

A failure always hands the ticket to humans exactly once; none is stuck.
Reference: [operations, AI handoffs](../operations.md#ai-handoffs-and-provider-failures),
[ai-usage](../ai-usage.md).

## Restore mode was left on

**Symptom.** Admins see "Restore mode is on: this is a restored copy…", `/api/ready`
shows `"restore_mode": true`, the alert `TikkitRestoreModeLeftOn` fires, and no email is
sent, no AI call is made, nothing is auto-closed, and SMTP and AI connection tests
answer "sends nothing to mail servers or AI providers", retention runs are refused.

**Cause.** A restored copy was promoted without leaving restore mode.

**Fix.** Finish the [post-restore checklist](../backup-restore.md#post-restore-checklist-before-any-live-work),
then recreate the stack **without** the restore overlay and with the production env
file (`docker compose --env-file .env.production -p tikkit -f docker-compose.prod.example.yml up -d`),
carrying over the database passwords as described in
[going live](../backup-restore.md#going-live-after-a-recovery). Reference:
[operations, backups](../operations.md#backups).

## Too many requests (HTTP 429)

**Symptom.** "Too many requests. Please wait a minute and try again." (`rate_limited`,
with a `Retry-After` header).

**Causes and fixes.**

- Legitimate bursts: the limits are per minute (failed sign-ins per client network,
  per username and network, per username; invitations, recovery, ticket submission,
  uploads, exports and so on per account). Wait a minute.
- **Everyone behind your proxy is limited together:** `TRUSTED_PROXIES` does not list
  the proxy and web server, so every client appears to come from one address. Set it to
  the subnet shared by the proxy and web (the reference file does) and recreate the API.
- Counters live in the API process memory and reset on restart.
- Do not run more than one API replica; limits are per process.

Reference: [security, bounded resource use](../security.md#bounded-resource-use),
[auth](../auth.md#abuse-protection).

## Stored credentials are unreadable after a restore or key change

**Symptom.** The AI page says the stored key cannot be read; the Outbound email page
shows "The stored mail password could not be read". Ticketing otherwise works and new
tickets skip AI.

**Cause.** The API runs with an `ENCRYPTION_KEY` different from the one that encrypted
the credentials (restore onto a host with a new key, a typo, or a deliberate rotation).
A missing or malformed key makes the API refuse to start instead.

**Fix.** If you still have the old key, put it back in `.env.production` and
`compose up -d --wait`. Otherwise enter the AI provider key and the SMTP **username and
password** again; they are re-encrypted with the current key. Nothing else needs repair.
`compose run --rm --no-deps api credentials-check` reports `readable`, `unreadable` or
`none` for each without sending anything. Keep the current key stored apart from
backups. Reference: [AI configuration](../ai-configuration.md#encryption-key-rotation-and-recovery),
[backup and restore](../backup-restore.md#when-encryption_key-is-missing-or-wrong-after-a-restore).

## A save fails or the session expires while someone is typing

**Symptom.** A form shows an error, the portal says "Your session has expired.", or
the page is offline; users worry that their text is lost.

**Behavior.** Typed input stays in the form. After a refused submit, focus moves to
an error summary that links to each field; a network or server failure shows its own
message and a retry; retrying unchanged input reuses its idempotency key, so no
duplicate ticket, reply or decision appears. For an expired session the user signs
in again in the new tab the banner offers and submits again. If every save is
refused, see [`csrf_rejected`](#every-save-is-refused-with-csrf_rejected). Reference:
[accessibility, failure recovery](../accessibility.md#failure-recovery).

## Other quick answers

| Symptom | Cause and fix |
| --- | --- |
| "An administrator must set up at least one category and one priority level first." on the ticket form | Create a category and a priority level in **Ticket catalog**. |
| Invitation link says invalid, expired, revoked or used | Links work once and expire. Reissue from **Accounts**. |
| Someone else could reach `/setup` first | Setup works once. Keep the portal closed to others until the first Admin exists ([install](install.md#5-create-the-first-admin)). |
| A screenshot preview fails with an error after a restore | The database row has no file in the restored volume. Restore a backup where both match ([backup and restore](../backup-restore.md#consistency-between-the-database-and-the-screenshots)). |
| A ticket appears to be "stuck" in AI | The deadline hands it to the queue; check API logs for `ai_attempts` and the [AI runbook](../operations.md#ai-handoffs-and-provider-failures). |
| Closed tickets cannot be reopened | **Allow reopening human-closed tickets** is off by default, and AI-solved tickets can never be reopened ([admin guide](admin.md#reopening)). |
| Backup sidecar failing | `compose logs backup \| grep level=error`; see [failure alerts](../backup-restore.md#failure-alerts). |
