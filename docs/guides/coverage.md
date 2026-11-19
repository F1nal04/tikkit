# Guide coverage map

Each acceptance criterion of the documentation issue
([#51](https://github.com/F1nal04/tikkit/issues/51)) and where the guides meet it.
[install](install.md), [requester](requester.md), [help desk](operator.md),
[admin](admin.md), [AI](ai.md), [troubleshooting](troubleshooting.md), [index](README.md).

| # | Criterion (short) | Where |
| --- | --- | --- |
| 1 | Fresh install: data services, migrations, admin access, production routing, backups, with working commands | [install](install.md) steps 1–5 and 8 |
| 2 | Requester guidance: category, external AI processing, mandatory AI, Solved/Still need help, failure fallback, read-only AI-solved history, not reopenable/escalatable, skip only when enabled | [requester: AI assistance](requester.md#ai-assistance-only-if-your-organization-uses-it); [AI: what the requester experiences](ai.md#what-the-requester-experiences) |
| 3 | Admin: global AI enablement, Allow requester to skip AI (default off), per-category Skip AI automatically, precedence, audit, reporting | [admin: AI controls](admin.md#ai-controls-and-their-precedence); [AI: mandatory AI, skipping and exemptions](ai.md#mandatory-ai-skipping-and-exemptions) |
| 4 | Operator: queues, team ownership, notes, replies, states, resolution, admin-controlled reopening | [help desk](operator.md) (Find your work, Work a ticket, Reopening) |
| 5 | Admin: AI-solved finality, requester read-only view, statistics, key storage/rotation/removal, budgets, permissions, retention | [admin: AI-solved records](admin.md#ai-solved-records), [permissions](admin.md#permissions-at-a-glance), [retention](admin.md#data-retention-and-personal-data); [AI: set it up](ai.md#set-it-up), [budgets](ai.md#budgets-and-safeguards), [finality and statistics](ai.md#finality-and-statistics) |
| 6 | Deployment, upgrade, rollback, restore, incident runbooks link to verified procedures and supported versions | [install](install.md#9-upgrade) (upgrade, rollback limits, restore); [index: supported versions](README.md#supported-versions); [troubleshooting](troubleshooting.md) |
| 7 | No real credentials; AI not implied mandatory or automatic | Whole set: placeholders only ([conventions](README.md#conventions)); [AI key points](ai.md); [admin](admin.md#ai-controls-and-their-precedence) |
| 8 | Allow reopening operator-resolved tickets toggle: default off, who may act, AI-solved exception | [admin: Reopening](admin.md#reopening); [help desk](operator.md#work-a-ticket); [requester](requester.md#reopen-a-closed-ticket-only-if-allowed) |
| 9 | Initial priority from admin-defined levels; operators raise/lower, history, queue effect; SLA is later scope | [requester: create a ticket](requester.md#create-a-ticket); [help desk: change priority](operator.md#change-priority); [index: release limitations](README.md#release-limitations) |
| 10 | All human-bound tickets enter the shared general queue; manual dispatch; history, conflicts, timestamps; no auto-routing | [help desk: where tickets come from](operator.md#where-tickets-come-from), [dispatch](operator.md#send-a-ticket-to-a-team-dispatch) |
| 11 | Visibility boundary, immediate loss of access, transfer confirmation, requester keeps access | [help desk: dispatch](operator.md#send-a-ticket-to-a-team-dispatch); [requester: where your ticket goes](requester.md#where-your-ticket-goes) |
| 12 | Dispatch without choosing an individual; claim or Team Lead/Admin assignment; no round-robin | [help desk: claim, assign and reassign](operator.md#claim-assign-and-reassign) |
| 13 | First setup, invite-only onboarding, role preselection (User default), one-time link, later role change, five role names | [install: first Admin](install.md#5-create-the-first-admin); [admin: accounts and invitations](admin.md#accounts-and-invitations) |
| 14 | The five roles; Specialist label and Team Lead do not bypass visibility, admin boundaries or AI-solved finality | [help desk: the five roles](operator.md#the-five-roles) |
| 15 | One team and at most one led team per non-admin; Admin never joins/leads/claims; separate accounts | [help desk: roles](operator.md#the-five-roles); [admin: what an Admin is](admin.md#what-an-admin-is-and-is-not), [teams](admin.md#teams-and-leadership) |
| 16 | Exactly one Team Lead per team; atomic Admin-only replacement; sole leader needs replacement or archive | [admin: teams and leadership](admin.md#teams-and-leadership) |
| 17 | Portal-only creation; outbound SMTP (host/port, sender, TLS, write-only credentials, test, rotation, disablement, safe errors); emails link to the portal | [admin: outbound email](admin.md#outbound-email); [requester: notifications](requester.md#notifications); [troubleshooting](troubleshooting.md#smtp-test-fails-or-no-email-arrives) |
| 18 | OpenAI, Gemini, Anthropic: setup, credentials, models, safe failure; later providers distinguished | [AI: supported providers](ai.md#supported-providers), [set it up](ai.md#set-it-up); [index: release limitations](README.md#release-limitations) |
| 19 | AI inputs: category, description, visible answers, screenshots; knowledge base is future | [AI: what is sent](ai.md#what-is-sent-to-the-provider-and-what-is-not); [index](README.md#release-limitations) |
| 20 | Deadlines, calendars, SLA breach and reminders are later; confirmation timeout is first-release and separate; no SLA setup required | [index: release limitations](README.md#release-limitations); [admin: resolution confirmation timeout](admin.md#resolution-confirmation-timeout) |
| 21 | Primary guide is self-hosted Docker Compose: environment/secrets, volumes, migration order, first Admin, origin/TLS, versioned images, SMTP/AI, logs/health, rollback limits | [install](install.md) |
| 22 | Screenshot-only attachments, formats and limits, visibility, storage/backups, AI analysis; no general uploads | [admin: screenshots](admin.md#screenshots); [requester: create a ticket](requester.md#create-a-ticket); [AI](ai.md#what-is-sent-to-the-provider-and-what-is-not) |
| 23 | One shared base form, selection-driven fields incl. program name, disclosure, vision-capable models, limits, failure; no unconditional skipping | [admin: ticket catalog](admin.md#ticket-catalog); [requester](requester.md#create-a-ticket); [AI: set it up](ai.md#set-it-up) |
| 24 | One-solution AI interaction: Solved / Still need help; no chat or regeneration; retries are not a second solution | [requester](requester.md#ai-assistance-only-if-your-organization-uses-it); [AI: what the requester experiences](ai.md#what-the-requester-experiences) |
| 25 | Human resolution: proposal, accept/reject, automatic closure; Still not solved is not reopening; AI-solved vs human resolution vs reopening | [requester: proposals](requester.md#when-support-proposes-a-resolution); [help desk: propose resolution](operator.md#work-a-ticket) |
| 26 | Admin timeout setting: timer start, deadline, closure reasons, prospective edits, reopening toggle, scope | [admin: resolution confirmation timeout](admin.md#resolution-confirmation-timeout) |
| 27 | Cancellation policy, mandatory reason, retained history, effects on active work and timers, distinct from solved/closed/reopened | [requester: cancel](requester.md#cancel-a-ticket); [admin: cancellation](admin.md#cancellation) |
| 28 | Claimed-ticket read/write rights, former assignee, requester actions vs staff editing | [help desk: claim, assign and reassign](operator.md#claim-assign-and-reassign) |
| 29 | Requesters edit drafts, frozen after submission, public replies and screenshots, no new AI solution | [requester: create a ticket](requester.md#create-a-ticket), [reply](requester.md#reply-and-add-information) |
| 30 | Username login, non-unique contact email, separate Admin and operational credentials, invitation and recovery per account | [requester: sign in](requester.md#sign-in-and-your-account); [admin: accounts](admin.md#accounts-and-invitations) |
| 31 | Everyone submits only for themselves | [requester: create a ticket](requester.md#create-a-ticket); [help desk: roles](operator.md#the-five-roles); [admin](admin.md#what-an-admin-is-and-is-not) |
| 32 | Staff cannot handle tickets they submitted, including Team Leads and Admins | [help desk: roles](operator.md#the-five-roles) |
| 33 | Self-handling restriction is per account, not per person | [help desk: roles](operator.md#the-five-roles); [admin](admin.md#what-an-admin-is-and-is-not) |
| 34 | Requesters see handling team and assignee name, with general-queue/unassigned states | [requester: where your ticket goes](requester.md#where-your-ticket-goes) |
| 35 | English/German selector and preference, fallback, localized notifications, AI language, content stays in original language | [requester: sign in](requester.md#sign-in-and-your-account); [admin: language](admin.md#language); [index](README.md) |
| 36 | SvelteKit/Svelte and Rust/Axum, check/test commands, Node-adapter build/start, dev proxy, production routing | Repository [README](../../README.md) (Commands, Production), [AGENTS.md](../../AGENTS.md), [deployment: runtime services](../deployment.md#runtime-services-and-probes), [frontend](../frontend.md); [index: how Tikkit is arranged](README.md#how-tikkit-is-arranged). No separate architecture page: those sources are complete and were corrected where stale. |

## Related evidence

- The tests behind each workflow the guides describe: [workflow coverage](../workflow-coverage.md).
- Accessibility behavior described in the requester and help desk guides (error summary,
  focus, announcements, session expiry): [accessibility](../accessibility.md), with the
  human screen-reader pass still outstanding.
- Host sizing and performance: [performance baseline](../performance-baseline.md).
