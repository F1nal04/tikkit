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
| SSRF through AI or SMTP settings | AI: fixed official hosts only, no redirects, private ranges refused. SMTP: Admin-only; TLS required and verified; link-local (cloud metadata), unspecified, multicast and broadcast hosts refused on save and after DNS at send time | `ai/http.rs`, `smtp::forbidden_destination`, `mail::send_once` | `ai_providers::private_destinations_need_an_explicit_policy`, `ai_providers::redirects_are_never_followed`, `boundary::smtp_settings_refuse_metadata_destinations_and_header_injection`, `smtp` unit test |
| Mail header injection | Sender name/address and recipients reject control characters and extra addresses; test sends go only to the acting Admin's or the sender address | `smtp.rs`, `auth::valid_email` | `boundary::smtp_settings_refuse_…` |
| Forged inbound mail or webhooks | There is no inbound channel: no route accepts mail- or webhook-shaped input, nothing creates, replies to, or reopens a ticket except the authenticated endpoints in the matrix | router | `boundary::there_is_no_inbound_email_or_webhook_channel`, `authz_matrix::*` |
| Spoofed client IP | `X-Forwarded-For` is honoured only from `TRUSTED_PROXIES` peers, read right to left; malformed entries never become the address | `security::TrustedProxies` | `security` unit tests, `boundary::forwarding_headers_count_only_from_trusted_proxies` |
| Forged request IDs | The public proxy strips `X-Request-ID`; the API accepts only a UUIDv4 and otherwise generates one ([operations.md](operations.md)) | proxy, `telemetry::http_request` | `lib` `request_ids_are_generated_or_propagated`, `observability::*` |
| Brute force and abuse | Sign-in limits per IP and username; per-account route budgets; caps on bodies, pages, exports, uploads, drafts; request deadline (below) | `auth.rs`, `security.rs` | `auth::login_is_rate_limited_per_username_and_ip`, `accounts::token_endpoints_are_rate_limited_per_ip`, `boundary::costly_routes_have_per_account_budgets`, `boundary::open_drafts_are_capped_per_account` |
| Username enumeration | Login failures are one `401 invalid_credentials` with equal timing; recovery always `202`; setup after completion always `409`; invitation lookups need the secret token | `auth.rs`, `accounts.rs` | `auth::login_failures_are_indistinguishable`, `accounts::recovery_is_neutral_and_bound_to_one_account` |
| Privilege escalation, cross-team access | Rust authorization on every route (below) | `authz.rs` | [Authorization matrix](#authorization-matrix) |
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

The web server's pages have no form actions or endpoints with business logic.
SvelteKit's own check still refuses cross-site `POST`/`PUT`/`PATCH`/`DELETE`
form submissions with `403`. With adapter-node 6 (SvelteKit 3) there is no
runtime `ORIGIN` variable: the page origin is `https://` plus the `Host` the
proxy forwards (or `PROTOCOL_HEADER`/`HOST_HEADER` when set), so the proxy must
pass the public host name unchanged.

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
microphone, geolocation, payment and USB, and HSTS when the page URL is https.
Static build assets are served by adapter-node before hooks run: the proxy
should add `X-Content-Type-Options: nosniff` for them.

## Reverse-proxy trust

`TRUSTED_PROXIES` ([configuration.md](configuration.md)) lists the peers whose
`X-Forwarded-For` the API believes: the public proxy and the SSR server. For
those peers the header is read from the right, skipping trusted hops; the first
other address is the client. Anything a client wrote to the left is ignored, a
malformed entry stops the walk, and a header from any other peer is ignored
entirely, so a forged header cannot reset a rate limit or change the IP in an
audit event. `X-Forwarded-Proto` and `X-Forwarded-Host` are never read by the
API: links, cookies and HSTS derive from `PUBLIC_ORIGIN`.

For the production stack (#46):

- Put the proxy, web, and API on a network with a fixed subnet and set
  `TRUSTED_PROXIES` to it (the example Compose file uses `172.30.0.0/24`).
- The proxy overwrites `X-Forwarded-For` with the peer address (Caddy's default
  without `trusted_proxies`), strips `X-Request-ID` on `/api/*`, passes `Host`,
  and adds `nosniff` for static assets. Keep header-read and idle timeouts at
  the proxy: the API itself has no slow-header timeout.
- The SSR server forwards the incoming `X-Forwarded-For` to the API; it must
  only be reachable through the proxy.
- Publish neither the API, the web server, nor the metrics port.

## Bounded resource use

| Resource | Bound |
| --- | --- |
| JSON bodies | 1 MiB (`413 payload_too_large`) |
| Screenshot uploads | 10 MiB, 40 MP, 30 s body read, two decodes at a time per process, 50 pending per account |
| Any API request | 60 s, then `503 request_timeout` (handler cancelled, transaction rolled back) |
| Lists | `limit` clamped to 1–100, keyset cursors |
| Search | `q` at most 200 characters, literal matching |
| Bulk | 1–50 tickets |
| Exports | 10,000 rows; report ranges at most 366 days |
| Drafts | 100 unsubmitted per account; intake field lengths in [submission.md](submission.md) |
| Saved views | 50 per account |
| AI | Admin budgets, concurrency, circuit breaker ([ai-usage.md](ai-usage.md)); provider timeouts |
| SMTP | 15 s per attempt, at most 3 attempts |

Per-minute budgets (`429 rate_limited` with `Retry-After`), fixed windows:

| Scope | Key | Limit |
| --- | --- | --- |
| Sign-in, setup, password change, invitation acceptance, recovery | client IP, then username | 10 / 5 |
| Invitation lookup, password reset | client IP | 10 |
| Ticket submission (direct and from a draft) | account | 10 |
| Draft create/save | account | 60 |
| Replies and internal notes | account | 30 |
| Screenshot uploads | account | 30 |
| Queue lists and search | account | 120 |
| CSV exports (reports, audit) | account | 6 |
| AI and SMTP connection tests | account | 5 |

The budgets are spent before the body is read and only by signed-in requests.
The counters live in memory, per API process, bounded to 10,000 live keys per
table (sign-in and route budgets use separate tables). Running several API
processes multiplies the effective limits; a shared store would be needed then.

## Authorization matrix

`apps/api/tests/authz_matrix.rs` takes every operation from the OpenAPI
document, checks that the router's `.route(...)` paths are exactly the
documented ones and that every undocumented method on them is `405`, and then
calls each operation as signed out, a User, an Operator, a Specialist, a Team
Lead and an Admin against one scenario: the User's ticket dispatched to a team
and claimed by the Operator, with the Specialist a non-assignee member and the
Team Lead its lead. Each row states the expected outcome per caller (allowed,
`401`, `403`, `404`). Nested resources (activity, messages, replies, notes,
assignment, staff actions), screenshot metadata/preview/download, counts,
search, bulk, every Admin export, and the activity poll (the only "event
stream") are rows.

**A new route fails `every_route_has_a_matrix_row` until it has a row.** For
new Admin endpoints (e.g. retention or personal-data export) the row is
usually `"UFFFFA"`; point the target at a throwaway resource if the Admin call
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
