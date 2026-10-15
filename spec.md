# Tikkit product specification

Status: agreed product requirements; implementation is tracked separately in GitHub.
Last updated: 2026-10-15.
Roadmap: [F1nal04/tikkit#2](https://github.com/F1nal04/tikkit/issues/2).

## Product and architecture

Tikkit is a full IT ticketing system for **one organization per deployment**. It provides a requester portal, a shared help-desk intake queue, team-based support, administration, and optional AI assistance before human support.

The repository uses Bun and Turborepo, a TanStack Start frontend, and a Rust backend. PostgreSQL is the default database, with one supported migration path. The first release targets self-hosting on one host with Docker Compose, persistent database and screenshot storage, and same-origin web/API routing.

The service desk must work without AI credentials or outbound email configuration. AI assistance is an optional organization capability using an Admin-managed bring-your-own-key (BYOK) credential.

## Accounts and onboarding

- First setup creates the initial Admin exactly once. Concurrent setup attempts and restarts cannot reopen bootstrap registration.
- After setup, account creation requires an Admin-issued invitation. There is no public signup.
- An Admin can preselect the invitation role. It defaults to User and is applied server-side when account creation succeeds; the invitee cannot override it.
- Invitation links are single-use, revocable, and time limited. They are consumed only after successful account creation, never by opening the link or a failed validation attempt. Concurrent acceptance has one winner.
- Admins may change roles after registration. Protect the last active Admin against removal or demotion.
- Accounts have immutable IDs and unique normalized usernames. Login uses username and password.
- Email is a contact and notification address, not a unique account identifier. Multiple accounts and invitations may share an email.
- Shared email never merges accounts, roles, credentials, sessions, or permissions. Administration and audit history distinguish accounts by account ID and username.
- Password recovery identifies the account by username, sends to its stored email, and uses a single-use token bound to that account. Recovery and deactivation affect only that account's relevant sessions and avoid account enumeration.
- Passwords are securely hashed. Sessions support expiry, rotation, logout, and server-side revocation.

When outbound SMTP is configured, it delivers invitations and recovery messages. Without SMTP, Admins can copy invitation links and distribute them themselves.

## Roles and team structure

The five roles are:

- **User:** submits and follows their own tickets.
- **Operator:** the basic help-desk role.
- **Specialist:** the same permissions as Operator, with an expertise/field designation. The title grants no additional access.
- **Team Lead:** manages and assigns tickets across their own led team, including tickets assigned to another team member.
- **Admin:** organization-wide administration, configuration, and ticket access, subject to ticket lifecycle invariants.

Each non-admin account may belong to at most one team and lead at most that same team. Each active team has exactly one Team Lead. Team creation and leader appointment are atomic; replacing the leader must not create co-leads or leave an active team without a leader. Block removal, demotion, deactivation, or transfer of the sole lead until a replacement is provided or the team is archived.

Admin accounts cannot belong to a team, lead a team, self-claim team tickets, or be individual team-ticket assignees. A person who performs both Admin and operational duties uses separate accounts. Those accounts may share an email.

Only Admins manage roles, memberships, leadership, categories, and organization settings.

### Self-submission and self-handling

All roles submit tickets only for their own signed-in account. There is no submission on behalf of another person and no later requester reassignment. The backend derives ownership from the authenticated account.

The submitting account cannot perform staff handling on its own ticket, even if it has Operator, Specialist, Team Lead, or Admin role. It cannot claim or be assigned the ticket, dispatch it, send staff replies or internal notes, change staff-controlled fields, or propose resolution. Its permitted requester actions remain available.

This restriction compares account IDs, not people or email addresses. A person may use a separate Admin account to handle a ticket submitted from their operational account, under normal Admin permissions. The Admin account still cannot become a team member or individual team-ticket assignee.

## Languages

The first release supports **English and German** across setup, authentication, requester workflows, staff workflows, Admin screens, validation, accessibility text, and system notifications.

Provide language selection before login and a persisted preference per account, with English as the fallback. Switching language preserves unsent input and never changes workflow state or permissions. Format dates and numbers appropriately while storing timestamps in UTC and keeping machine-readable role/status/error identifiers stable.

System emails use the intended recipient's applicable language. User-authored descriptions, replies, names, and historical AI suggestions remain as written; changing the interface language does not automatically translate them.

## Categories and the shared ticket form

Every category uses one shared base form:

- Category.
- Subject.
- Problem description.
- Requester-selected priority.
- Optional screenshots.

Admins manage categories, priority levels and ordering, and simple selection-driven conditional fields. For example, selecting a software issue can reveal a program selector with a free-text alternative.

Conditional fields are required only while visible. Rust validates applicable rules and excludes hidden or stale answers from committed intake and AI context. This is a limited conditional-field mechanism, not a general-purpose nested form or workflow builder.

Archived categories and choices disappear from new-ticket forms while historical tickets retain readable snapshots. Category selection never automatically chooses a support team.

The requester chooses the initial priority directly from Admin-defined levels. Authorized staff can increase or decrease the effective priority. Preserve the original selection and every subsequent change in history.

### Drafts and submission

Requesters can save and edit their own drafts. Drafts are absent from staff queues and have an explicit retention policy.

Submission validates ownership, category, priority, visible conditional answers, screenshot references, and limits server-side. Persist a stable ticket/reference ID before AI processing or human handoff. Idempotency and transactional state changes prevent duplicate tickets from retries or double clicks.

Submission freezes the original intake snapshot and screenshot references. Requesters cannot later edit or delete the submitted details, replace original screenshots, or rewrite earlier messages. They may add public replies and new screenshots on permitted open tickets. Later additions do not regenerate AI assistance.

Closed, cancelled, and AI-solved records do not accept new content. A human-handled closed ticket may accept content again only after an authorized reopen under the current Admin policy.

## Screenshots

First-release attachments are screenshots in validated static PNG, JPEG, or WebP format. General files, documents, archives, SVG/HTML, and executable uploads are outside scope.

Validate actual decoded content, count, bytes, dimensions, and safe filenames; reject corrupt, spoofed, and excessively large images. Provide accessible previews, clear failures, and draft removal before submission. Expire orphan uploads safely.

Store files in a persistent private filesystem volume and metadata in PostgreSQL. Preview and download endpoints recheck current ticket/message access. Never publish private image URLs solely so an AI provider can fetch them.

Public-reply screenshots follow public ticket visibility. Internal-note screenshots follow staff-only visibility. Team changes and membership removal revoke subsequent image access. Backups and restores include files and their database records consistently.

## Optional AI assistance

### Providers and configuration

The initial supported providers are **OpenAI, Google Gemini, and Anthropic**. Admins configure the selected provider/model and its organization credential, test the connection, replace/remove credentials, and enable or disable AI.

AI defaults to disabled. Credentials are encrypted server-side using an externally supplied encryption key, write-only through the UI/API, redacted from errors and logs, and bound to the intended provider. Supported model configuration must account for screenshot/image-input capability.

### AI policy and routing

Evaluate policy server-side before dispatching a provider call:

1. Globally disabled or unconfigured AI routes directly to human support.
2. An Admin can mark a category **Skip AI automatically**, default off. Exempt categories route directly to human support with zero provider calls.
3. Otherwise, AI is mandatory before human support. Requesters cannot skip it by default.
4. An independent organization toggle, **Allow requester to skip AI**, defaults off. When enabled by an Admin, it permits a requester-controlled skip into human support.

Provider failures, timeouts, incompatible image/model configuration, exhausted budgets, and unavailable credentials must preserve a usable human handoff. Failure cannot trap the requester behind mandatory AI. All human-bound paths route the same persisted ticket exactly once.

Whole-ticket cancellation is separate from skipping AI: a permitted cancellation terminates the request rather than sending it to operators.

### Inputs and the single solution

The initial immutable AI input contains:

- Selected category and problem description.
- Relevant currently visible conditional answers, such as program name.
- Authorized screenshots attached to the submitted intake.
- The selected English or German solution language.

Do not include requester identity, other tickets, unrelated files, hidden/stale answers, internal notes, or internal-note images. Knowledge-base grounding is deferred.

Screenshots must be sent as actual supported image inputs to a vision-capable model. Validate and normalize images, strip unnecessary metadata, and enforce application/provider limits. Do not silently omit an image and claim it was analyzed.

Each submitted request receives **one proposed solution**, which may contain ordered steps. There is no AI chat, follow-up conversation, regeneration, or try-another-solution action. Bounded transport retries belong to one persisted logical attempt and cannot expose multiple completed solutions.

Request the solution in the requester's chosen English or German language and store that language with the attempt. Refreshing, switching interface language, or adding later replies/screenshots does not generate another solution.

### Requester decision

After the persisted suggestion, the requester chooses:

- **Solved:** record an explicit AI resolution and its confirmation event.
- **Still need help:** send the same ticket, original context, and allowed AI context to the shared general operator queue.

AI output alone, page views, navigation away, silence, and provider completion never mark a ticket solved. Unconfirmed and abandoned attempts remain distinguishable from success.

AI-solved records are final. Their owner can read them in history; Admins can inspect them and include them in statistics. They never enter operator lists, detail, search, counts, exports, attachments, or events. No role or setting can reopen or escalate them, and they accept no further replies or edits.

### Reliability and safety

Persist attempts and decisions, use durable jobs, bound retries, and handle concurrent or stale decisions transactionally. Late provider results cannot overwrite a committed handoff, cancellation, or final decision. Admin policy changes may stop pending work without accepting stale results.

Apply request/token budgets, concurrency limits, circuit breaking, input/output bounds, safe output rendering, and secret redaction across all three providers. Treat text and screenshots as untrusted input. Do not execute suggested actions or log private prompts, image bytes, or raw ticket content by default. Disclose external processing before submission without introducing an unconditional skip option.

## Human support queues and permissions

Every new ticket requiring human support enters **one shared general queue**, with no team or individual assignee. Category, priority, and AI outcome never automatically route it to a team.

An authorized Operator or Specialist manually dispatches it to an active team. The ticket leaves general intake and retains its reference, original intake, screenshots, messages, priority history, AI context, and timestamps.

Team dispatch leaves the individual assignee empty. An eligible active member can claim it; that team's Team Lead or an Admin can explicitly assign it to an eligible member. There is no automatic or round-robin assignment. Concurrent claims/transfers have one winner.

After dispatch, staff access is limited to current destination-team members and Admins. The dispatcher and former team members lose access if they are not members of the destination team. Enforce this on detail, search, counts, exports, attachments, caches, realtime events, and queued notifications. Return a safe transfer receipt if the dispatcher loses access.

For a claimed ticket, staff writes require the current assignee, its team's Team Lead, or an Admin. Other team members are read-only and cannot take over another member's ticket. Team Leads and Admins can explicitly reassign work; the previous assignee loses write permission immediately. Requester actions and defined system jobs have separate permissions. The self-handling rule still applies.

Authorized team transfers and returns to general intake preserve history and clear ineligible individual assignments. Search, saved filters, and bounded bulk actions enforce the same visibility and per-ticket write rules.

Requesters retain access to their own public ticket information. They can see the handling team's name and assigned staff member's display name, including clear general-queue and unassigned states.

## Conversations and resolution

Tickets support public replies and private internal notes. Requesters never receive internal notes or staff-only screenshots through the portal, email, exports, or other responses.

Keep these states distinct: draft, AI processing, awaiting AI decision, queued, in progress, waiting for requester information, awaiting requester confirmation of a staff resolution, closed, and cancelled. Record immutable transition events and resolution provenance separately from current state.

### Staff-proposed resolution

Staff propose a resolution with a required public summary. This enters **Awaiting requester confirmation** and remains visible to the assigned team and Admins. It is distinct from the pre-triage AI decision state.

The owning requester can:

- Confirm resolution, closing the ticket with an explicit confirmation event.
- Select Still not solved, returning the same ticket to active support with its existing team and history.

Rejecting a proposal before closure is not reopening and does not require the reopening toggle. Version proposals and guard against duplicate, stale, and concurrent decisions. A plain reply or viewed notification does not confirm resolution.

### Automatic closure

If the requester does not respond to a staff resolution proposal, close it after an Admin-configured positive elapsed period, **7 days / 168 hours by default**, measured from the proposal.

Snapshot the proposal version, timeout, and deadline; setting changes affect subsequent proposals. Show the deadline to the requester and staff. Rejection, cancellation, or a superseding proposal invalidates the old timer.

Durable processing must survive restarts, catch up overdue proposals, and handle multiple workers and decision races without duplicate closure or notifications. Record system-driven confirmation-timeout closure separately from requester-confirmed closure. Silence never counts as acceptance or AI success.

This first-release timeout is independent of deferred SLA functionality. It is not a generic inactivity timeout and does not close queued tickets, waiting-for-information tickets, or pending AI decisions.

### Reopening

Reopening human-handled closed tickets is controlled by an Admin toggle, **off by default**. When enabled, authorized actors can reopen under the current policy. Enabling the toggle never reopens tickets automatically. Alternate endpoints, replies, jobs, and stale clients cannot bypass it.

AI-solved records remain permanently non-reopenable regardless of this setting.

### Requester cancellation

The owning requester can cancel a submitted open ticket with a mandatory nonblank bounded reason. Admins choose one policy:

- **Anytime while open**, the default.
- **Only before work starts**.
- **Disabled**.

Before work starts means before the first recorded human work-start/In Progress event; unassignment or transfer does not reset that history. Recheck policy and state when committing.

Cancellation retains the ticket, reason, actor, time, and history, removes active work, and cancels pending AI/confirmation jobs. Late results cannot resurrect it. It is neither deletion nor a solved outcome. Final closed and AI-solved records cannot be cancelled.

## Email and notifications

Ticket creation and replies are **portal-only**. There is no inbound-email ticket intake, email-driven reply, or email-triggered lifecycle change.

Admins can configure an outbound SMTP service such as Strato using host, port, sender identity, credentials, and verified TLS/STARTTLS, with safe connection testing and enable/disable controls. Credentials are encrypted, write-only, and excluded from logs and responses.

Use a transactional outbox and durable retries for invitation, recovery, submission, assignment, public-reply, resolution, cancellation, and closure notifications. Email links return users to the portal; replies must be posted there. Delivery failure does not roll back ticket work.

Recipients are account-specific, including accounts sharing an email. Revalidate access at delivery, suppress messages to staff who lost access, and never include internal content in requester notifications. Support notification preferences and localized system text.

## Administration, reporting, and data protection

Admins manage accounts, invitations, roles, teams, categories, priorities, conditional fields, AI policies/credentials, SMTP, and lifecycle settings.

A separate Admin submission view includes pending/abandoned AI attempts, final AI-solved records, escalated tickets, human resolutions, and cancellations. Merely viewing a record must not assign or mutate it.

First-release reporting includes volumes, categories, priorities, workload, observed response/resolution times, AI outcomes/usage, and bounded exports. Distinguish AI-confirmed resolution, requester-confirmed human closure, automatic confirmation-timeout closure, cancellation, failure, and unconfirmed attempts. Do not fabricate SLA deadlines or treat silence as resolution confirmation.

Persist attributable audit events for authentication, invitations, roles, memberships, settings, ticket transitions, priorities, assignment, AI decisions, exports, and retention. Use immutable account identities, UTC timestamps, safe metadata, and atomic events or durable outbox records. Secrets do not enter audit payloads; normal application actions cannot rewrite audit history.

Provide explicit retention for drafts, screenshots, ticket content, AI data, and audit metadata, with scoped exports and audited deletion/anonymization. Preview destructive retention actions and preserve appropriate aggregate statistics and historical relationships. Explain backup retention and deletion limits.

## Deployment, CI, and verification

### CI

Take inspiration from `../bookr/.github/workflows/ci.yml`: one Ubuntu `checks` job, pull requests and main pushes, read-only permissions, concurrency cancellation, and root-level commands.

Adapt the backend tooling to Rust and install the required Bun, Node, and Rust versions. Pin action SHAs. Run frozen dependency installation, then root `check`, `build`, and `test` tasks with locked Cargo dependencies. All PRs, including automation PRs, receive checks. Document/configure the stable required check where repository permissions allow it.

Add grouped dependency updates and vulnerability checks. Keep the workflow small; extra jobs, matrices, and caches require a demonstrated need.

### Production operations

Provide versioned frontend/API images, required background processing, PostgreSQL, a TLS-capable reverse proxy, private service networking, and persistent database/screenshot volumes in Docker Compose. Required processing includes AI jobs when enabled, the notification outbox, and resolution-confirmation timeout jobs; it need not imply one container per job type.

Document clean installation, secrets, migrations, first-admin setup, readiness, graceful shutdown, TLS, upgrades, rollback limits, and supported versions. Container replacement preserves data. No Kubernetes, hosted platform subscription, or external object store is required.

Back up database and screenshots consistently, encrypt backups, keep copies off the application host, and manage secret/encryption-key custody separately. Verify recovery into an isolated Compose project with AI and email dispatch disabled. Record measured recovery objectives and handle restored jobs without duplicate notifications.

### Quality gates

Build authorization, validation, audit events, English/German localization, and tests alongside each feature. Production readiness completes their cross-cutting verification; it is not permission to defer the basics.

Use real PostgreSQL integration tests, deterministic local AI/provider fixtures, and a local SMTP test service. Automated tests require no real provider credentials or paid email service. Cover every initial provider's multimodal request shape and failure behavior.

Verify complete requester/staff/Admin workflows, role/team isolation, shared-email account separation, self-handling rules, concurrency, job restart recovery, backup restoration, and failure handoff. Test both languages, keyboard/screen-reader use, mobile/desktop layouts, safe output, session/CSRF boundaries, and recoverable errors. Target WCAG 2.2 AA expectations for critical workflows.

## Release milestones

1. [Foundation and CI](https://github.com/F1nal04/tikkit/milestone/1): checks, test tooling, persistence, API contracts, runtime configuration, onboarding, and permissions.
2. [Core service desk](https://github.com/F1nal04/tikkit/milestone/2): a complete no-AI requester/staff workflow, administration, screenshots, team dispatch/assignment, conversations, notifications/SMTP, and resolution-confirmation auto-close.
3. [Optional BYOK AI assistance](https://github.com/F1nal04/tikkit/milestone/3): three initial providers, multimodal single-solution assistance, requester decisions, reliable escalation, and usage/privacy safeguards. Establish safeguards before enabling generation.
4. [Production readiness](https://github.com/F1nal04/tikkit/milestone/4): complete Admin reporting/audit views, security/accessibility verification, production deployment, backups, documentation, and release validation.

Issue dependencies define prerequisites; issue numbers are not an execution order. The first release includes all four milestones. Optional AI means deployments can leave it disabled, not that its agreed implementation is deferred.

## Deferred scope

These features do not block the first release:

- Additional hosted AI providers and self-hosted models beyond OpenAI, Gemini, and Anthropic.
- Knowledge-base authoring, requester browsing/search, and optional AI grounding.
- SLA targets, business calendars, deadlines, breach reporting, reminders, and escalation automation. Basic timing statistics and the specific resolution-confirmation timeout ship initially.
- Enterprise OIDC SSO with explicit secure linking to invited accounts. It must not bypass onboarding, auto-provision roles, or merge accounts by email.

Multi-organization SaaS tenancy/billing, inbound email, arbitrary file attachments, general-purpose form builders, automatic category-to-team routing, automatic individual assignment, and conversational/regenerating AI are outside the agreed first-release scope.

## Release acceptance

Release validation must prove the complete no-AI workflow, mandatory and exempt AI paths, authorized skipping, explicit final AI resolution, failure handoff, team isolation, staff/requester confirmation, seven-day-default auto-close, cancellation, and Admin-controlled human-ticket reopening.

Also verify invite-only onboarding, all five roles, one-team/one-lead constraints, separate shared-email accounts, account-specific self-handling restrictions, English/German workflows, screenshot understanding integrations, production persistence, and an isolated restore. CI must pass for the release commit, with traceable artifacts and documentation matching the delivered behavior.

See [the roadmap](https://github.com/F1nal04/tikkit/issues/2) and [release checklist #52](https://github.com/F1nal04/tikkit/issues/52) for implementation tracking and detailed verification.
