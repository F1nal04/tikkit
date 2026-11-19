# Security boundary

What protects the portal at the web/API boundary, where it lives, and which
test proves it. Feature-level rules stay in their own pages
([auth](auth.md), [authorization](authorization.md), [screenshots](screenshots.md),
[AI providers](ai-providers.md), [audit](audit.md)); this page is the
threat-oriented inventory and the record of the release review (#44).

Deployment shape: one public reverse proxy (TLS) sends `/api/*` to the Rust API
and everything else to the SvelteKit server. The SSR server calls the API
directly over the private network. Only the proxy publishes ports.

## Threats and controls

| Threat | Control | Where | Tests |
| --- | --- | --- | --- |
| Cross-site request forgery against the API | Every `POST`/`PUT`/`PATCH`/`DELETE` needs `X-Requested-With: tikkit` (forces a CORS preflight, never granted); a browser's `Origin` must equal `PUBLIC_ORIGIN` and `Sec-Fetch-Site`, when sent, must be `same-origin`; `SameSite=Lax` cookie | `auth::csrf`, `security::same_origin` | `boundary::authenticated_mutations_need_the_header_and_the_portal_origin`, `auth::state_changes_require_the_csrf_header` |
| Cross-site form posts to the web server | SvelteKit's origin check (default on, never relaxed); pages have no form actions | SvelteKit (`vite.config.ts` keeps `csrf` default) | `e2e/security.spec.ts` |
| Login CSRF / session fixation | Login is a mutation under the same policy; a new session per login, presented cookies are revoked | `auth::login` | `boundary::authenticated_mutations_…`, `auth::signing_in_again_ends_the_presented_session` |
| Session theft | `HttpOnly; SameSite=Lax; Path=/`, host-only, `Secure` in production; only a SHA-256 is stored; rotation and revocation | `auth.rs` | `boundary::every_session_cookie_is_hardened_in_production`, `auth::*` |
| Reading API data cross-origin | No CORS headers at all (preflights are `405`), `Cross-Origin-Resource-Policy: same-origin` | `security::headers` | `boundary::every_api_response_carries_security_headers_and_no_cors_grant` |
| XSS in the portal | Svelte text interpolation only (no `{@html}`), plain-text bodies; nonce CSP without `unsafe-inline`/`unsafe-eval` for scripts; `object-src 'none'`, `base-uri 'none'` | `vite.config.ts`, `hooks.server.ts` | `e2e/security.spec.ts`, `e2e/requester.spec.ts` (intake/reply payloads), `e2e/admin-audit.spec.ts`, `e2e/ai-step.spec.ts`, `messages::plain_text_is_escaped_and_messages_and_images_are_append_only` |
| Clickjacking | `frame-ancestors 'none'` and `X-Frame-Options: DENY` on pages and API | both | same |
| MIME sniffing / active content via uploads | Magic-byte detection, full re-encode, SVG/HTML/PDF/polyglots rejected; served with stored type, `nosniff`, `Content-Disposition`, `sandbox` CSP, never a public URL | `screenshots.rs` | `screenshots::spoofed_corrupt_animated_oversized_and_disallowed_files_are_rejected`, `screenshots::each_supported_format_is_decoded_reencoded_and_served_safely` |
| Spreadsheet formula injection | Every export cell goes through `csv::cell` | `csv.rs` | `csv` unit test, `admin_submissions::csv_neutralizes_dangerous_prefixes_in_all_three_name_columns`, `audit::export_is_permissioned_bounded_neutralized_and_audited` |
| Prompt injection through ticket text or images | The model sees only the frozen, minimized intake (`ai_privacy::minimize`); its output is stored as plain text and can change nothing: routing is manual, only the requester's explicit decision ends AI, no tools, no URLs fetched, no other ticket's data in the request | `ai_privacy.rs`, `ai_attempts.rs`, `docs/ai-decisions.md` | `ai_attempts::later_replies_and_screenshots_never_reach_the_provider`, `ai_guard::secrets_and_ticket_content_reach_neither_the_provider_storage_nor_logs`, `ai_decisions::*`, `e2e/ai-step.spec.ts` |
| SSRF through AI or SMTP settings | AI: fixed official hosts only, no redirects, private ranges refused. SMTP: Admin-only; TLS and hostname verification required; an operator-provided `SMTP_CA_FILE` replaces the default SMTP trust roots with a private CA bundle; link-local (cloud metadata), unspecified, multicast and broadcast addresses, `fd00:ec2::254`, and IPv6 forms embedding such an IPv4 address (mapped, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) are refused on save and after DNS at send time; host names with a numeric last label (`2852039166`, `0xa9fea9fe`) are refused on save. Loopback and private relays stay allowed | `ai/http.rs`, `smtp::forbidden_destination`, `mail::send_once` | `ai_providers::private_destinations_need_an_explicit_policy`, `ai_providers::redirects_are_never_followed`, `boundary::smtp_settings_refuse_metadata_destinations_and_header_injection`, `smtp::a_host_resolving_to_a_refused_address_is_never_contacted`, `smtp` unit test |
| Mail header injection | Sender name/address and recipients reject control characters and extra addresses; test sends go only to the acting Admin's or the sender address | `smtp.rs`, `auth::valid_email` | `boundary::smtp_settings_refuse_…` |
| Forged inbound mail or webhooks | There is no inbound channel: no route accepts mail- or webhook-shaped input, nothing creates, replies to, or reopens a ticket except the authenticated endpoints in the matrix | router | `boundary::there_is_no_inbound_email_or_webhook_channel`, `authz_matrix::*` |
| Spoofed client IP | `X-Forwarded-For` is honoured only from `TRUSTED_PROXIES` peers (no network wider than /8 or /16), read right to left, ports stripped; malformed entries never become the address | `security::TrustedProxies` | `security` unit tests, `boundary::forwarding_headers_count_only_from_trusted_proxies` |
| Forged request IDs | The public proxy strips `X-Request-ID`; the API accepts only a UUIDv4 and otherwise generates one ([operations.md](operations.md)) | proxy, `telemetry::http_request` | `lib` `request_ids_are_generated_or_propagated`, `observability::*` |
| Brute force, abuse, targeted lockout | Sign-in failures limited per client network, per username and network, and per username (sparing the owner's known network); per-account route budgets; caps on bodies, pages, exports, uploads, drafts; request deadline (below) | `auth.rs`, `security.rs` | `auth::login_is_rate_limited_per_username_and_ip`, `auth::sign_in_limits_count_failures_and_cannot_lock_out_the_owner`, `auth` unit tests, `boundary::a_stuck_request_ends_at_the_deadline_and_commits_nothing`, `accounts::token_endpoints_are_rate_limited_per_ip`, `boundary::costly_routes_have_per_account_budgets`, `boundary::open_drafts_are_capped_per_account` |
| Username enumeration | Login failures are one `401 invalid_credentials` with equal timing (an unusable stored hash, such as an anonymized account's, is verified against the dummy hash too); recovery always `202`; setup after completion always `409`; invitation lookups need the secret token. Deactivated and anonymized accounts, by old username or `anonymized-` placeholder, answer exactly like unknown ones | `auth.rs`, `accounts.rs` | `auth::login_failures_are_indistinguishable`, `accounts::recovery_is_neutral_and_bound_to_one_account`, `boundary::deactivated_and_anonymized_accounts_look_unknown_when_signed_out`, `auth` unit test `unusable_hashes_cost_a_full_verification_and_never_match` |
| Privilege escalation, cross-team access | Rust authorization on every route (below) | `authz.rs` | [Authorization matrix](#authorization-matrix) |
| Retention and erasure misuse | Admin only (`ManageSettings` for policy, preview, runs; `ManageAccounts` for personal-data summary, export, anonymization); a run or automatic deletion needs a fresh preview token and the typed `DELETE`; anonymization needs a deactivated account and its typed username; budgets on preview, run start, export and anonymization; every removal authorized inside the database by a pending run or an anonymized requester ([retention.md](retention.md)) | `retention.rs`, `personal_data.rs`, migration `0028` | matrix rows, `boundary::costly_routes_have_per_account_budgets`, `retention::*` |
| Data left after removal | A screenshot removed by retention answers every read and delete exactly like an ID that never existed, for every caller; detail responses list only existing screenshot IDs (`removed_screenshots` counts the rest, only for callers who may see the ticket) | `screenshots.rs`, `tickets.rs` | `boundary::a_screenshot_removed_by_retention_answers_like_one_that_never_existed`, `retention::detail_views_offer_neither_removed_screenshots_nor_a_reopen_that_cannot_work` |
| Personal-data export abuse or injection | JSON written from PostgreSQL's own JSON text, at most 16 MiB (`413 export_too_large`, measured before loading) and 10,000 rows per section; an attachment named only by the account ID, with the API's `nosniff`/CSP headers; no credentials, hashes, tokens or internal notes | `personal_data.rs` | `boundary::the_personal_data_export_is_inert_json_whatever_the_account_wrote`, `retention::*` export tests |
| Secret disclosure | AI keys and SMTP credentials encrypted, write-only, never logged or audited | `ai_config.rs`, `smtp.rs`, `audit.rs` | `ai_config::keys_are_encrypted_masked_replaceable_and_removable`, `smtp::admin_stores_the_password_encrypted_and_never_returns_it`, `audit::secrets_and_ticket_content_never_reach_the_log` |
| Vulnerable dependencies | `cargo audit`, `bun audit --audit-level=high`, gitleaks, Dependabot | CI | [security-scanning.md](security-scanning.md) |

## CSRF and origin policy

One policy for every state-changing API request, applied by middleware before
any handler: the custom header **and**, if the browser sent them, a matching
`Origin` and `Sec-Fetch-Site: same-origin`. Anything else is
`403 csrf_rejected` and changes nothing. Callers without browser metadata (the
SSR server, scripts, tests) are judged by the header alone; a browser cannot
send the header cross-site without a preflight the API never answers. Safe
methods (`GET`, `HEAD`, `OPTIONS`) never change state.

`PUBLIC_ORIGIN` is the anchor: the portal must be opened exactly under that
origin, in development too (`http://localhost:3000`; `127.0.0.1` or a LAN
address gets `403 csrf_rejected` on every save, and the message tells the
user to open the configured address). The check stays strict on purpose:
allowing other origins would let any page on them write as the user.

The web server's pages have no form actions or endpoints with business logic.
SvelteKit's own check still refuses cross-site `POST`/`PUT`/`PATCH`/`DELETE`
form submissions with `403`. With adapter-node 6 (SvelteKit 3) there is no
runtime `ORIGIN` variable. Unless the build sets `kit.paths.origin` (we do
not: one image serves every deployment), the page origin `event.url.origin`
is `https://` plus the request's `Host` (`HOST_HEADER` / `PROTOCOL_HEADER`
change the source). Two things use it today: SvelteKit's form-post origin
check, and `handleFetch`, which sends SSR fetches to `/api/*` on that origin
(and only those) to `API_ORIGIN` with the session cookie. Both rely on the
reverse proxy accepting only the configured site host and passing it
unchanged: a proxy that answered any `Host` would make the checked origin
whatever the client sent.

## Security headers

API (`security::headers`, every response including errors): `nosniff`,
`X-Frame-Options: DENY`, `Content-Security-Policy: default-src 'none';
frame-ancestors 'none'; base-uri 'none'; form-action 'none'`,
`Referrer-Policy: no-referrer`, `Cross-Origin-Resource-Policy` and
`-Opener-Policy: same-origin`, `Cache-Control: private, no-store`, and
`Strict-Transport-Security: max-age=31536000` when `PUBLIC_ORIGIN` is https.
Screenshot files keep their stricter own CSP (`sandbox`) and disposition.

Pages (`hooks.server.ts`, SvelteKit `csp`): a nonce-based CSP —
`default-src 'self'`, `script-src 'self' 'nonce-…'`, `style-src 'self'`
(nonce for inline elements), `style-src-attr 'unsafe-inline'` (SvelteKit's
route announcer uses a style attribute), `img-src`/`font-src`/`connect-src
'self'`, `frame-src`/`object-src 'none'`, `base-uri 'none'`,
`form-action 'self'`, `frame-ancestors 'none'` — plus the same nosniff,
framing, referrer, COOP/CORP headers, a permissions policy denying camera,
microphone, geolocation, payment and USB. Pages send HSTS only when
`PROTOCOL_HEADER` is configured and the request came in as https: without it
adapter-node merely assumes https, which proves nothing.

Some web responses never pass the hooks: static build assets (served by
adapter-node first), SvelteKit's own plain-text `403` for refused form posts,
and the static fallback error page. The reverse proxy must therefore add
`X-Content-Type-Options: nosniff` and `Content-Security-Policy:
frame-ancestors 'none'` to every response it serves (a second identical
`frame-ancestors` is harmless where the app already set its CSP).

## Reverse-proxy trust

`TRUSTED_PROXIES` ([configuration.md](configuration.md)) lists the peers whose
`X-Forwarded-For` the API believes: the public proxy and the SSR server. For
those peers the header is read from the right, skipping trusted hops; the first
other address is the client. Anything a client wrote to the left is ignored, a
malformed entry stops the walk (an entry with a port, `1.2.3.4:5678` or
`[::1]:5678`, is read without it), and a header from any other peer is ignored
entirely. Networks wider than IPv4 /8 or IPv6 /16 are a configuration error,
so no list can trust "everyone" by halves; IPv4-mapped IPv6 networks are the
IPv4 network they denote. All of this means so a forged header cannot reset a rate limit or change the IP in an
audit event. `X-Forwarded-Proto` and `X-Forwarded-Host` are never read by the
API: links, cookies and HSTS derive from `PUBLIC_ORIGIN`.

### Requirements for the production stack (#46)

1. Proxy, web, and API share a network with a fixed subnet; the API's
   `TRUSTED_PROXIES` is that subnet (the example Compose file uses
   `172.30.0.0/24`). Nothing else joins it.
2. The proxy serves only the configured site host (`PUBLIC_HOST`) and passes
   `Host` unchanged; requests for any other host get no portal (Caddy's site
   block does this).
3. The proxy overwrites `X-Forwarded-For` with the peer address (Caddy's
   default without `trusted_proxies`) and `X-Forwarded-Proto`, and strips
   `X-Request-ID` on `/api/*`.
4. The web server runs with `PROTOCOL_HEADER=x-forwarded-proto` (pages send
   HSTS) and without `ORIGIN` (ignored by adapter-node 6; harmless if left).
5. `PUBLIC_ORIGIN` is `https://<PUBLIC_HOST>`, exactly what users open.
6. The proxy adds `X-Content-Type-Options: nosniff` and
   `Content-Security-Policy: frame-ancestors 'none'` to every response
   (static assets, SvelteKit's own `403`, fallback error page), and may add
   HSTS with `includeSubDomains`/`preload` if the whole domain is https-only.
7. Header-read, body and idle timeouts are set at the proxy (the API has no
   slow-header timeout of its own; its request deadline is 60 s, so proxy
   upstream timeouts should be longer than that).
8. The SSR server forwards the incoming `X-Forwarded-For` to the API; it must
   only be reachable through the proxy.
9. Neither the API, the web server, nor the metrics port is published.

## Bounded resource use

| Resource | Bound |
| --- | --- |
| JSON bodies | 1 MiB (`413 payload_too_large`) |
| Screenshot uploads | 10 MiB, 40 MP, 30 s body read, two decodes at a time per process, 50 pending per account |
| Any API request | 60 s, then `503 request_timeout` (handler cancelled, transaction rolled back) |
| Lists | `limit` clamped to 1–100, keyset cursors |
| Search | `q` at most 200 characters, literal matching |
| Bulk | 1–50 tickets |
| Exports | 10,000 rows; report ranges at most 366 days; personal-data export 16 MiB and 10,000 rows per section |
| Retention runs | one at a time; 100 tickets or drafts per transaction, the batch stops after 1,000 changed rows; audit purge 5,000 per transaction ([retention.md](retention.md#preview-and-runs)) |
| Drafts | 100 unsubmitted per account; intake field lengths in [submission.md](submission.md) |
| Saved views | 50 per account |
| AI | Admin budgets, concurrency, circuit breaker ([ai-usage.md](ai-usage.md)); provider timeouts |
| SMTP | 15 s per attempt, at most 3 attempts |

Per-minute budgets (`429 rate_limited` with `Retry-After`), fixed windows:

| Scope | Key | Limit |
| --- | --- | --- |
| Sign-in, password change: failures only. Setup, invitation acceptance, recovery: every request | client network (IPv4 address, IPv6 /64) / username + network / username over all networks, except networks the account signed in from in the last 30 days | 10 / 5 / 30 |
| Invitation lookup, password reset | client IP | 10 |
| Ticket submission (direct and from a draft) | account | 10 |
| Draft create/save | account | 120 (autosave is debounced at 1.2 s; a refused save keeps the input and retries after 5 s) |
| Replies and internal notes | account | 30 |
| Screenshot uploads | account | 30 |
| Queue lists and search | account | 120 |
| CSV exports (reports, audit) and the personal-data export | account | 6 |
| AI and SMTP connection tests | account | 5 |
| Retention preview and run start | account | 30 |
| Account anonymization | account | 10 |

Route budgets are spent before the body is read and only by signed-in
requests. Sign-in checks run before any account lookup and answer known and
unknown usernames identically.

**Lockout trade-off.** A per-username limit alone lets anyone lock a known
account by failing five times a minute. Instead the tight limit (5) is per
username *and* client network, so an attacker locks only themselves. The
per-username ceiling (30 a minute over all networks) still slows a guessing
botnet, which needs many networks to reach it; while it is reached, everyone
but the owner's known-good networks (successful sign-in there in the last 30
days, remembered per process) waits for the window to pass. IPv6 is counted
per /64, so rotating addresses inside one allocation buys nothing.

The counters live in memory, per API process, bounded to 10,000 keys per
table (sign-in, route budgets and known-good networks are separate tables). A
full table drops expired windows and then the oldest one, so new clients are
never refused for want of room; flushing a victim's counter that way takes
thousands of fresh networks per minute. Running several API processes
multiplies the effective limits; a shared store would be needed then.

## Authorization matrix

`apps/api/tests/authz_matrix.rs` takes every operation from the OpenAPI
document, checks that the router's `.route(...)` paths are exactly the
documented ones and that every undocumented method on them is `405`, and then
calls each operation as signed out, a User, an Operator, a Specialist, a Team
Lead, an Admin, and two strangers against one scenario: the User's ticket
dispatched to a team and claimed by the Operator, with the Specialist a
non-assignee member and the Team Lead its lead; `other` is a User who is not
the requester and `outsider` an Operator of a second team. Every ticket-scoped
row, the requester's draft, pending upload and notification, and the
Operator's saved view must deny both strangers. Each row states the expected
outcome per caller: allowed (a `2xx` for reads; for writes a `2xx` or the
handler's own `4xx`, never `429` or `5xx`), `401`, `403`, `404`; a row can pin
an exact status (deleting an attached original is `409`), and bulk rows are
judged by the per-ticket result. An operation can have rows for several
resources (attached and pending screenshots).

Route discovery reads every file under `apps/api/src`: it collects the literal
`.route("/api/...")` paths of `lib.rs`, requires them to equal the OpenAPI
paths, and fails on anything it could not see (`.nest`, `.merge`,
`.route_service`, fallbacks, routers outside `lib.rs`, non-literal paths); the
private `/metrics` listener is the one named exception. Nested resources (activity, messages, replies, notes,
assignment, staff actions), screenshot metadata/preview/download, counts,
search, bulk, every Admin export, and the activity poll (the only "event
stream") are rows.

**A new route fails `every_route_has_a_matrix_row` until it has a row.** For
new Admin endpoints (e.g. retention or personal-data export) the row is
usually `"UFFFFAFF"` (as for the retention and personal-data routes, whose
rows use an all-"keep" policy, a run token that matches no preview, and an
active account that anonymization must refuse with `422 account: active`);
point the target at a throwaway resource if the Admin call
changes state.

The scenarios the issue lists are covered by focused tests:

| Requirement | Tests |
| --- | --- |
| Team isolation after dispatch, transfer, membership removal, stale assignment | `assignment::transfer_and_return_move_visibility_everywhere`, `messages::transfer_and_membership_removal_revoke_conversations_and_note_images_immediately`, `messages::note_image_access_cannot_combine_old_ticket_team_with_new_membership`, `management::removal_clears_assignment_audits_it_and_revokes_staff_access_but_not_requester_access`, `screenshots::attached_screenshots_follow_current_ticket_visibility`, `notifications::access_is_rechecked_when_delivered`, `concurrency::activity_stops_for_lost_access_and_revoked_sessions`, `lifecycle::staff_writes_recheck_assignee_leadership_and_role_at_commit`, `queues::sql_visibility_matches_ticket_view_for_every_actor_and_ticket`, `search::search_matches_every_field_under_staff_visibility` |
| Team read vs claimed-ticket write (assignee, their Lead, Admin) | matrix rows for staff actions, assignment, replies, notes; `operator::claimed_tickets_are_writable_only_by_assignee_lead_and_admin`, `messages::staff_writes_follow_assignment_and_self_handling_rules_and_derive_authorship`, `search::bulk_management_is_limited_to_the_lead_of_each_team_or_admin`, `search::bulk_claim_applies_per_ticket_and_reports_partial_failures`, `messages::private_conversation_responses_cannot_be_cached_after_access_revocation` (stale sessions) |
| Self-handling denied for every staff role | `lifecycle::the_requester_never_handles_their_own_ticket_in_any_role`, `operator::self_handling_is_denied_by_account_id_only`, `assignment::self_handling_is_refused_by_account_id`, `concurrency::self_handling_is_forbidden_before_the_stale_check`, `ai_decisions::a_staff_account_decides_only_as_the_requester_of_its_own_ticket` |
| AI-solved records out of every operator path | `ai_decisions::ai_stage_and_ai_solved_records_are_absent_from_every_operator_path`, `ai_decisions::no_role_can_mutate_an_ai_solved_record`, `lifecycle::ai_solved_is_final_through_every_path`, `notifications::ai_solved_records_only_notify_the_requesters_own_confirmation`, `assignment::ai_solved_and_finished_records_are_excluded` |
| Requester immutability (intake, earlier replies, original screenshots) | `boundary::requester_cannot_rewrite_submitted_content`, matrix row `DELETE /api/screenshots/{id}` (`409` for the uploader of an attached image), `messages::plain_text_is_escaped_and_messages_and_images_are_append_only`, `submission::submission_freezes_intake_and_routes_once_with_ai_disabled`, `submission::stale_and_submitted_drafts_cannot_be_edited_deleted_or_resubmitted`, `screenshots::uploads_stay_private_until_submission_freezes_them`, `lifecycle::database_backstops_reject_inconsistent_lifecycle_rows` |
| One team, one lead, Admin never in a team (API, invitations, concurrent updates) | `db::admins_cannot_be_members_or_leads`, `db::an_account_belongs_to_at_most_one_team_and_history_is_kept`, `db::team_leads_are_unique_members_with_the_team_lead_role`, `db::concurrent_membership_and_admin_promotion_cannot_both_commit`, `management::competing_team_creations_cannot_share_a_lead_and_restore_requires_an_eligible_lead`, `management::teams_and_roles_preserve_leadership_and_immediately_change_live_session_scope`, `management::concurrent_admin_demotions_have_one_winner_and_preserve_last_admin`, `accounts::invitation_creates_the_account_with_the_server_side_role`, `boundary::role_payloads_cannot_carry_extra_authority` |
| Shared email never merges or selects accounts | `auth::accounts_sharing_an_email_stay_separate`, `audit::accounts_sharing_an_email_stay_distinct_in_history`, `accounts::recovery_is_neutral_and_bound_to_one_account`, `notifications::preferences_apply_per_account_and_each_account_gets_its_own_mail` (all fixtures share one email) |
| No username enumeration when signed out | `auth::login_failures_are_indistinguishable`, `accounts::recovery_is_neutral_and_bound_to_one_account`, `accounts::expired_and_revoked_invitations_are_rejected` |

## Manual review (2026-11-18, #44)

Focused read of the code paths, against the running test suite.

- **Authentication.** Checked session creation, rotation grace, revocation on
  password change/deactivation, cookie attributes in every `Set-Cookie` path,
  login timing for unknown users, recovery neutrality and its cooldown, the
  invitation acceptance lock, the CSRF middleware, and how the client IP feeds
  limits and audit. *Result:* the client IP came from the last
  `X-Forwarded-For` entry of **any** peer once `TRUST_PROXY_HEADERS=true`, so a
  caller reaching the API without the proxy could rotate the header to escape
  sign-in limits and write arbitrary text as the audited IP. Fixed with
  `TRUSTED_PROXIES`. The CSRF check relied on the custom header only; the
  browser's `Origin`/`Sec-Fetch-Site` are now enforced too.
- **Upload.** Checked type detection, re-encoding, decoder limits, the upload
  semaphore and body timeout, the pending cap and its lock, attach-once
  binding, deletion of attached images (refused), and serving headers.
  *Result:* no defect. Uploads had no per-account rate; now 30/min.
- **AI credentials.** Checked encryption with provider binding, write-only
  responses, redaction from errors/audit/logs, the connection test path, and
  the outbound destination policy. *Result:* no defect. The connection test
  (a paid provider call) had no rate; now 5/min, as is the SMTP test.
- **Queue isolation.** Checked `staff_visible_sql` against `ticket_view`, the
  queue/count/search/bulk/saved-view queries, the activity poll, screenshot
  reads through ticket visibility, and notification rechecks. *Result:* no
  defect; the new matrix confirms the per-role outcomes route by route.
- **Resource bounds.** *Result:* drafts were unbounded per account (each up
  to 20,000 characters plus answers); now capped at 100. No API request had
  an overall deadline; now 60 s.
- **Web.** *Result:* pages had no CSP, framing protection or nosniff; added
  and tested against the production build. SvelteKit 3's adapter-node no
  longer reads `ORIGIN`; documented above for the deployment.

### Follow-up review (2026-11-18)

An independent review of this branch found no bypass of the origin policy,
proxy trust, budgets, or CSP. Fixed from it: the matrix could not see a
missing object-level check (strangers added, "allowed" tightened); the
deadline and send-time DNS check had no failing test without them; route
discovery missed non-literal routes; `TRUSTED_PROXIES` accepted `/1` halves,
mis-masked IPv4-mapped networks and stopped at `ip:port` entries; sign-in
limits allowed targeted lockouts, keyed IPv6 per address, counted successes,
and refused new clients when full; SMTP refusal missed NAT64, 6to4, the EC2
IPv6 metadata address and numeric host names; page HSTS trusted adapter-node's
https assumption; two busy autosaving tabs could exceed the draft budget, and
a refused save waited for the next keystroke.

### Retention and personal data (#43, 2026-11-19)

Read `retention.rs`, `personal_data.rs`, migration `0028` and
[retention.md](retention.md) against the boundary
controls above.

- **Ordering with the account lock.** Every personal-data writer takes
  `auth::lock_active_account` first. The draft cap runs after it (an inactive
  or anonymized account is refused with `401` before anything is counted);
  sign-in and password budgets are in-memory checks before any lookup, so they
  neither hold nor wait for the lock and answer alike for every username.
- **Enumeration.** *Result:* an anonymized account's stored hash is `!`, which
  failed verification without Argon2 work, so signing in with its placeholder
  was measurably faster than with an unknown username. Fixed in `auth::login`
  (unusable hashes are verified against the dummy hash). Deactivated and
  anonymized accounts now answer sign-in and recovery exactly like unknown
  usernames.
- **Authorization and budgets.** All seven new operations are Admin only and
  in the matrix for all eight callers; preview, run start, export and
  anonymization have budgets. *Result:* no defect.
- **Removed screenshots.** *Result:* no defect: reads and deletes are `404`
  like an unknown ID for every caller, and detail views no longer name them.
- **Export.** *Result:* no injection: rows are PostgreSQL JSON text, the
  filename is the account ID, and the API headers apply. Noted: the
  export confirms the username with `SELECT … FOR UPDATE`, so while one is
  built (up to 16 MiB) the account's own writers (sign-in, profile, drafts)
  wait for it; and `GET /api/admin/personal-data?username=` puts a username in
  the URL, which a reverse proxy may write to its access log (keep URL logging
  off, as for invitation and reset links).

## Known limits

- Rate-limit counters are per process and reset on restart.
- The API has no slow-header (slowloris) timeout of its own; the proxy's applies.
- An API caller that bypasses the proxy can still choose its request ID (a
  valid UUIDv4); keep the API unpublished.
- `style-src-attr 'unsafe-inline'` remains for SvelteKit's announcer; style
  attributes cannot run script and markup is never injected.
- SMTP host names are checked after DNS before connecting; a rebinding between
  that check and the connection is still bound by verified TLS.
- HSTS is sent without `includeSubDomains`/`preload`; add them at the proxy if
  the whole domain is https-only.
- The known-good network memory is per process: after a restart, the owner is
  subject to the per-username ceiling until they sign in once.
- SMTP: only the well-known NAT64 prefix is decoded; a site-specific NAT64
  prefix (RFC 8215 local use) is judged as a plain IPv6 address.
