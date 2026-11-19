# Administrator guide

For the people who set up and run a Tikkit organization. It assumes the stack
is installed ([install guide](install.md)) and you are signed in as an Admin.
Administration is under **Administration** in the sidebar: Overview,
Submissions, Reports, Audit log, Data retention, Accounts, Teams, Organization
settings, Outbound email, AI assistance and Ticket catalog. Every change you make
is recorded in the [audit log](#audit-log).

The portal is available in English and German; this guide uses the English
labels. One deployment serves exactly one organization.

## What an Admin is, and is not

- Admins manage accounts, roles, teams, categories, priorities, settings, email,
  AI, reports, audit and retention. Only Admins can.
- Admins can see and work every dispatched ticket and read all submissions.
  **Admins never join teams, lead teams, claim tickets or become assignees.** An
  Admin's organization-wide visibility is not team membership.
- If a person does both Admin work and help desk work, they use **two accounts**
  (the accounts may share an email). Each has its own username, password and
  permissions; sharing an email links nothing.
- Admins submit tickets only for themselves, like everyone, and cannot handle a
  ticket their own account submitted. A *different* Admin account may handle it.
- The role names are exactly **User**, **Operator**, **Specialist**, **Team
  Lead** and **Admin**; see the [help desk guide](operator.md#the-five-roles).
- Protection: the last active Admin cannot be demoted or deactivated.

## First day checklist

Do these in order. Requesters cannot submit tickets until a category and a
priority level exist, and nothing needs email or AI.

1. **Create the first Admin** at `/setup` (once only; see [install](install.md#5-create-the-first-admin)).
2. **Organization settings**: name, timezone, default language, and the ticket
   policies (see [Organization settings](#organization-settings)).
3. **Ticket catalog**: at least one category and one priority level.
4. **Teams and people**: invite the Team Leads first, create teams, then invite
   everyone else ([Accounts](#accounts-and-invitations), [Teams](#teams-and-leadership)).
5. **Backups** ([install guide](install.md#8-set-up-backups-before-real-tickets)).
6. Optional: **Outbound email** and **AI assistance**.
7. Decide your **data retention** policy (nothing is deleted until you set one).

## Accounts and invitations

There is no public signup. After the first Admin, accounts exist only through
invitations.

**Invite someone:** **Accounts** → **Invite**.

1. Choose the **Role**. It is preselected as **User**; the invitee cannot change it.
2. Optionally enter an **Email (optional)** and **Valid for (hours)** (default
   168; 1–720).
3. Choose **Create invitation**. The link is shown **once**. Copy it and send it
   yourself, or, if outbound email is enabled and you entered an address, it is
   also emailed in the chosen language. A lost link cannot be recovered: revoke
   it or choose **Reissue invitation** (which invalidates the old link and creates a
   new one valid for 168 hours).

Invitation facts:

- A link is single-use, revocable and time-limited. It is consumed **only when
  the account is created successfully**, never by opening it or by a failed
  attempt (wrong username, weak password). If two people open it, one wins.
- The invitee chooses a username (unique, normalized; names starting with
  `anonymized-` are reserved) and a password, and enters a contact email. The
  role comes from the invitation, applied by the server.
- **Invitation history** lists pending, accepted, expired and revoked invitations
  (with the resulting account) and never shows tokens.
- Email is only a contact address. **Several accounts and invitations can share
  one email.** Accounts are never merged, linked or looked up by email. Identify
  accounts by username and account ID.
- **Password recovery** uses the username, sends a one-hour, single-use link to
  that account's stored email, and affects only that account. Without outbound
  email no recovery links are sent, and there is no Admin-side password reset in
  this release, so set up outbound email before relying on recovery. Signed-in
  users can change their own password under **Profile**.

**Change a role later:** **Accounts** → **Edit account**. Set **Role**, the
**Specialist expertise / field** (only for Specialists; descriptive, grants
nothing) and **Team**. Changes apply to live sessions immediately. Only Operators,
Specialists and Team Leads can join a team; Users and Admins have none. **Deactivate**
ends the account's sessions at once and refuses sign-in; its tickets and audit
history stay. **Reactivate** restores it. Deactivating the last active Admin, or
the lead of an active team, is refused.

## Teams and leadership

Rules the product enforces:

- A non-Admin account is in **at most one team** and can lead only that team.
- Each active team has **exactly one Team Lead**. Creating a team and appointing its
  lead is one atomic step, and replacing a lead is atomic too: there are never two
  leads or none.
- The sole lead cannot be removed, demoted, deactivated or moved until someone else
  leads or the team is archived. You see "This account leads an active team.
  Appoint another lead or archive the team first."
- The Team Lead role alone grants no leadership. You appoint the lead on the team.

**Create a team:** **Teams** → **Team name** and **Team Lead account** (choose an
active Team Lead who does not already lead a team) → **Create team**. The lead
becomes a member. **Add members:** **Accounts** → **Edit account** → **Team**.

**Replace the lead:** **Teams** → the team → choose another **Team Lead account** →
**Save**. The previous lead loses only the leadership designation and remains a
member. Choosing someone from another team moves that account into this team and
clears their assignments in the old one.

**Archive a team** (**Archive team**): this is the other way to retire a sole lead.
It clears the leadership, removes all memberships and assignments, and keeps ticket
and audit history. **Restore team** needs an eligible lead.

Tickets already sent to a team stay with it. Teams never receive tickets
automatically: operators dispatch by hand, and members claim or a Team Lead/Admin
assigns ([how tickets move](operator.md#send-a-ticket-to-a-team-dispatch)).
After dispatch, only that team's members and Admins have staff access.

## Organization settings

**Organization settings** edits these. Each change is audited.

| Setting | Default | Meaning |
| --- | --- | --- |
| Organization name | Tikkit | Shown in the portal. |
| Timezone | UTC | Display of invitation times. All timestamps are stored in UTC. |
| Default language | English | Language preselected for invitations. Existing accounts keep their own. |
| **Allow reopening human-closed tickets** | **Off** | Whether closed tickets that were handled by the help desk can be reopened. |
| **Requester cancellation** | **Anytime while open** | **Anytime while open**, **Only before work starts**, or **Disabled**. |
| **Resolution confirmation timeout (hours)** | **168** (7 days) | How long a staff resolution proposal waits for the requester before closing automatically. |
| **Allow requesters to skip AI assistance** | **Off** | Lets requesters skip an AI suggestion. See [AI controls](#ai-controls-and-their-precedence). |

### Resolution confirmation timeout

- **When it starts.** Each staff resolution proposal snapshots the timeout and a
  deadline: the proposal time plus the configured hours. The deadline is shown to
  the requester and to staff.
- **Edits apply to later proposals only.** Existing proposals keep their
  snapshotted deadline.
- **Closing.** If the requester confirms, the ticket closes as *confirmed by the
  requester*. If the deadline passes with no decision, a background job closes it
  as *closed automatically: no response before the confirmation deadline*.
  Reports and the Submissions list keep these two apart; silence is never counted as
  acceptance. If the requester chooses **Still not solved**, the ticket returns to
  active support. That is a rejection, not a reopening, and needs no setting.
- **Scope.** Only tickets **awaiting requester confirmation** have a deadline.
  Queued tickets, tickets waiting for the requester's information and tickets in
  an AI step are not subject to it. It is not an SLA feature.
- The job is durable: after a restart, overdue proposals close at the next poll.

### Reopening

With **Allow reopening human-closed tickets** off (the default), closed tickets stay
closed; requesters open a new ticket instead. When you switch it on:

- The requester of a closed ticket and a staff member who may work that ticket (its
  assignee, team lead or an Admin, never its own requester as staff) can reopen it,
  which returns it to In progress.
- It never reopens anything by itself, and it applies to tickets closed by the help
  desk, whether confirmed or closed automatically.
- Tickets **solved by AI can never be reopened or escalated by any role**, whatever
  this setting says. Cancelled tickets and tickets whose content was removed by the
  retention policy cannot be reopened either.

Switching it back off applies at once; the setting is read at the moment of each action.

### Cancellation

Requesters cancel their own submitted, open tickets, always with a reason (up to 1,000
characters). The policy decides when they may. **Only before work starts** means
before the first time staff set the ticket In progress; unassigning or transferring
a ticket does not reset that. Cancelled tickets stay in history with the reason,
actor and time, active work and pending AI or confirmation timers are cancelled, and
the result is final. Final closed tickets and AI-solved records cannot be cancelled.

## Ticket catalog

**Ticket catalog** defines what requesters see. Every category uses the same base
form: category, subject, description, priority, and optional screenshots.

- **Categories**: add, rename, reorder, archive and restore. Archived entries
  disappear from new forms, and existing tickets keep their frozen names.
  Category **Skip AI automatically** is per category (see below). **A category never
  selects a support team.**
- **Priority levels**: add, reorder, archive. Requesters pick the starting
  priority from these. **List the most urgent level first:** sorting the queue by
  priority uses this order. There are no response or resolution deadlines tied to a
  level.
- **Conditional fields**: small selection-driven extras, for example a **Program**
  field for software categories. A field has a **Label**, optional **Guidance**, a
  **Type** (**Text** or **Selection**), **Required while visible**, **Shown for
  categories**, and for selections **Choices** and **Allow a free-text answer instead
  of a choice** (the "Other (type your own answer)" option, used for a program that is
  not in your list). Answers are required only while the field is visible; hidden
  or stale answers are never saved with a ticket or sent to AI. This is not a form
  builder.

Add at least one category and one priority, or the ticket form says "An
administrator must set up at least one category and one priority level first."

## AI controls and their precedence

AI is **off by default** and **never required** to run the service desk. Setup is in
[the AI guide](ai.md). Three Admin controls decide the route of a **new** ticket,
checked in this order:

1. **Global AI assistance** (Administration → AI assistance). If AI is disabled,
   unconfigured, or unusable (missing or unreadable key, unsupported model), every
   ticket goes straight to the shared general queue. Nothing else is evaluated and
   no provider is contacted.
2. **Category "Skip AI automatically"** (Ticket catalog, default off). A flagged
   category goes straight to the general queue with **zero provider calls**, even
   when AI is on and even if requesters may skip.
3. Otherwise **AI is mandatory** before human support: the requester cannot skip.
4. **Allow requesters to skip AI assistance** (Organization settings, default **off**)
   is independent of the others and only matters at step 3. When on, the requester
   can skip at submission or while the suggestion is being prepared. It does not enable
   AI and does not change category exemptions.

Provider failures, timeouts, exhausted budgets or an unavailable credential never
trap a requester: the same ticket reaches the general queue exactly once.

**Audit trail.** The settings changes are recorded as `organization_settings_updated`,
`ai_settings_updated`, `ai_credential_set`, `ai_credential_removed`,
`ai_connection_tested` and `catalog_item_updated` (with the category's before
state). Each ticket's `ticket_submit` event records the route taken (`ai`,
`ai_disabled`, `category_exempt` or `requester_skip`); an AI failure and the
requester's decision are recorded too.

**Reporting impact.** Reports and Submissions count the routes separately:
**Category exempt from AI**, **AI disabled or unconfigured**, **Requester skipped
AI (allowed)**, plus the AI outcomes. Bypasses add to submission volume but not to
AI attempts or the AI confirmation rate. AI-solved tickets are counted only when the
requester explicitly confirmed. See [Reports](#reports-and-submissions).

### AI-solved records

Only an explicit requester **Solved** makes a ticket AI-solved. The record is **final
and non-reopenable for every role**: no one can reopen, escalate, reply to, edit,
dispatch or cancel it. The requester keeps read-only access. Operators never see
it in queues, search, counts, exports or notifications. Admins read it under
**Submissions** and **AI assistance → AI outcomes and records** and it is counted in
statistics. Attempts to change it are refused and logged as `ai_solved_change_denied`.
The retention policy can later remove its stored text and images, like any finished
ticket; the status and counts remain.

## Outbound email

Email is optional and outbound only. **Tickets are created and answered in the portal.**
There is no inbound email: replying to a notification email changes nothing. Emails
link to the portal and say so.

**Outbound email** → fill in:

| Field | Notes |
| --- | --- |
| **Host**, **Port** | For example your relay `smtp.<your-provider>` and port 587 or 465. (Strato, for instance, documents `smtp.strato.de`.) Check your provider's current settings. |
| **Transport security** | **STARTTLS (usually port 587)** or **Implicit TLS (usually port 465)**. The certificate is always verified; there is no plaintext option. |
| **Sender name**, **Sender address** | The identity on outgoing mail. Your provider may require it to match the account. |
| **Username**, **Password** | Stored encrypted with `ENCRYPTION_KEY`; never shown again and never logged. |
| **Enabled** | Off keeps the portal fully working; invitation links are copied by hand. |

Steps:

1. Allow the API's outbound DNS and TCP to the relay port in the host firewall.
2. Save, then **Send test email**. It goes only to your own account email or the sender
   address, and makes one attempt (up to 15 seconds). Success means the relay
   accepted the message.
3. Turn **Enabled** on.

Rotation: enter **both** username and password again (a password alone leaves the stored
username in place). Changing the **Host** requires entering both again, so stored
credentials are never sent to a new server. Disable with **Enabled** off, or **Remove
connection** to delete it. A wrong `ENCRYPTION_KEY` makes the stored credentials
unreadable; enter them again.

Errors are shown as stable messages, never the server's text: for example "The mail
server rejected the username or password", "Could not establish a verified TLS
connection to the mail server", "The mail server could not be reached", "The mail
server refused the message". Background delivery retries (a failure never undoes
ticket work); the page shows outbox counts and **Last delivery problem**, and you
can re-queue failed rows. Recipients are individual accounts, so accounts sharing an
email each get their own message, and mail to staff who lost access is suppressed.
See [troubleshooting](troubleshooting.md#smtp-test-fails-or-no-email-arrives).

## Screenshots

The first release accepts screenshots only: static **PNG, JPEG or WebP**, at most 10
MiB each, 10 per ticket, at most 16,384 px per side and 40 megapixels. Animated
images, SVG, PDF, archives and other files are rejected. Files are stored in the
private `screenshots-data` volume (never publicly addressable), served only through
the portal after an access check, and **backed up together with the database**.
Public-reply images follow the ticket's visibility; internal-note images are
staff-only. Access ends when a ticket moves to a team the viewer is not in. When AI
applies, intake screenshots are sent to the provider as images (see [AI](ai.md)).

## Reports and submissions

- **Submissions** lists every submitted ticket and what became of it (still with AI,
  AI-solved, escalated, resolved by the help desk, cancelled), with filters. Viewing
  never assigns, reopens or changes anything.
- **Reports** show submission volume, categories, priorities, workload, observed
  response and resolution times, and **AI outcomes** (confirmation and deflection
  rates). Closures are broken down into *confirmed by the requester*, *closed
  automatically* and *cancelled*, and AI confirmation is separate. There are no
  SLA targets or breach reports. CSV export covers up to 366 days and 10,000
  submissions and omits ticket text.
- **AI assistance → AI outcomes and records** lists AI attempts with their outcome
  (solved, not solved, skipped, failed, cancelled, still pending).

Definitions: [Admin reports](../admin-reports.md), [Admin submissions](../admin-submissions.md).

## Audit log

**Audit log** shows who did what and when: sign-ins, invitations, role and team
changes, settings, AI and SMTP changes, exports, retention and every ticket event. Entries
are attributed to account IDs, timestamped in UTC, contain no secrets or ticket text,
and cannot be edited. Filter by actor, action, ticket or request ID and export up to
10,000 rows as CSV (the export is itself logged). Details: [audit](../audit.md).

## Data retention and personal data

**Nothing is deleted by default.** **Data retention** has periods in days for drafts,
screenshots, AI inputs and suggestions, ticket content, and audit entries. An empty
period means keep forever, and automatic deletion is off. Audit entries are kept at
least 30 days.

To delete: set periods, choose **Preview deletion** (changes nothing), then
**Delete now** after typing `DELETE`. A run removes only the content of **finished**
tickets (closed, cancelled, AI-solved) past the period; the ticket, its history and
every statistic stay. AI-solved records follow the same periods. A removed ticket
cannot be reopened.

Also here: find one account by exact username to **Export personal data** (JSON) or
**Remove identity**, which anonymizes a *deactivated* account while keeping history and
statistics attached to an anonymous placeholder.

**Backups keep old data until they expire.** Deletion does not rewrite earlier
backups, and a restore brings removed data and identities back; redo retention
and anonymization after any restore ([backup and restore](../backup-restore.md#backups-and-deleted-data),
[retention](../retention.md)).

## Language

The product is available in English and German everywhere: sign-in, tickets,
staff and Admin screens, errors and notification emails. Every account has a
language preference (English fallback). Messages, subjects, replies and AI suggestions stay in
the language they were written in; changing the interface language does not translate
them. Emails use the recipient's language.

## Not in this release

SLA targets and deadlines, business calendars, breach reporting and reminders;
knowledge-base articles and AI grounding or citations; providers beyond OpenAI,
Google Gemini and Anthropic, and self-hosted models; enterprise SSO; inbound email;
arbitrary file attachments; multiple organizations per deployment; hosted-platform
and Kubernetes guides. The resolution-confirmation timeout above *is* in this release
and is separate from SLAs. See the [guide index](README.md#not-in-this-release).
