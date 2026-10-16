# Permissions

The role/permission matrix lives in `apps/api/src/authz.rs` as pure functions
(no database access) and is tested exhaustively in `apps/api/src/authz/tests.rs`.
Admin directory changes (roles, teams, leadership) are services in
`apps/api/src/admin/`. The database repeats the structural invariants with
constraints and triggers (`apps/api/migrations/0001_initial_schema.sql`).

Authorization happens on the server for every resource access. Client route
guards only help with navigation. The authenticated account is loaded from the
database on every request, so role changes, team changes and deactivation
take effect right away.

## Rules for endpoint authors

- Ticket endpoints (milestone 2) must call the matching `authz::can_*`
  function before they read or change a ticket, its messages, its screenshots,
  its events or its exports. Staff list, search, count and export queries must
  apply `authz::staff_ticket_scope` in SQL before pagination. A test checks
  that this scope matches `can_view_ticket_as_staff` row by row.
- The `/api/v1/admin/...` endpoints (`admin::http`, listed in
  [api.md](api.md#administration-admin-only)) take the `Actor` extractor,
  call `authz::can_manage_org` and then the `admin::accounts` /
  `admin::teams` services. Each service checks again, inside its own
  transaction, that the actor is an active Admin.
- Denials map to HTTP like this:

  | `Denial`        | HTTP | code                | Meaning                                                      |
  | --------------- | ---- | ------------------- | ------------------------------------------------------------ |
  | `NotVisible`    | 404  | `not_found`         | The actor may not know the resource exists                   |
  | `Forbidden`     | 403  | `forbidden`         | The actor can see it but may not do this                     |
  | `InvalidState`  | 409  | `conflict`          | Not possible in the current state (closed, AI-solved, claimed) |
  | `InvalidTarget` | 422  | `validation_failed` | The chosen assignee or destination team is not eligible      |

## Roles

| Role       | What it is                                                                 |
| ---------- | -------------------------------------------------------------------------- |
| User       | Requester. Submits and follows only their own tickets.                     |
| Operator   | Baseline help-desk role.                                                   |
| Specialist | **Identical** to Operator. The specialization is only a label. A test runs the whole matrix for both roles and checks the results are equal. |
| Team Lead  | Operator plus management of the **one team it leads**.                     |
| Admin      | Organization administration, plus staff access to tickets across all teams. |

Lead authority comes from the leadership flag on the account's current
membership, not from the role name. An account with the `team_lead` role that
is only a plain member of a team (for example a lead who was replaced) has
plain member permissions. A test covers this.

## Ticket matrix

"Member" means an active Operator, Specialist or Team Lead whose current
membership is the ticket's team. "Lead" means the designated lead of that team.
The table assumes someone else submitted the ticket and that it is in active
human work (queued, in progress, waiting for requester, or awaiting
confirmation).

| Action                                   | General queue             | Team, unclaimed        | Team, claimed            | Other team     |
| ---------------------------------------- | ------------------------- | ---------------------- | ------------------------ | -------------- |
| View, internal notes, staff screenshots  | all staff                 | members, Admin         | members, Admin           | Admin only     |
| Claim (self-assign)                      | — (dispatch first)        | members (not Admin)    | — (already claimed)      | —              |
| Assign / reassign to an eligible member  | —                         | lead, Admin            | lead, Admin              | Admin          |
| Unassign                                 | —                         | —                      | lead, Admin              | Admin          |
| Dispatch to a team                       | all staff                 | lead, Admin (transfer) | assignee, lead, Admin    | Admin          |
| Return to the general queue              | —                         | lead, Admin            | assignee, lead, Admin    | Admin          |
| Staff reply, note, fields, screenshots   | Admin                     | lead, Admin            | assignee, lead, Admin    | Admin          |
| Change priority                          | Admin                     | lead, Admin            | assignee, lead, Admin    | Admin          |
| Propose a resolution                     | Admin                     | lead, Admin            | assignee, lead, Admin    | Admin          |

"All staff" means Operator, Specialist, Team Lead and Admin. A golden table in
the tests pins these outcomes.

How we read the rules where the spec leaves room:

- **General queue:** the permission the spec establishes for the queue is
  dispatch. So non-admin staff can view and dispatch, and every other staff
  change there is Admin-only.
- **Unclaimed team ticket:** plain members can read it and claim it. Other
  staff changes need the team's lead or an Admin, so a member claims the ticket
  first and then works on it.
- **Claimed team ticket:** only the current assignee (who must still be a
  current member), that team's lead, or an Admin may change it. Other members
  can read it but not change it. Past assignment or past authorship of
  messages gives no extra rights.
- **After dispatch:** staff access is limited to current members of the
  destination team plus Admins. The dispatcher loses access unless it belongs
  to that team. Assignment never bypasses team membership: an assignee who
  moved to another team loses access.

## Self-handling

Staff never handle tickets they submitted themselves. That holds for every
role, including Team Lead and Admin. The check compares account IDs only,
never people, usernames or email addresses.

- Every staff function returns `Forbidden` on the actor's own ticket: staff
  view, internal content, claim, assign, dispatch, writes, priority and
  proposals. The submitting account sees the ticket through the requester
  view, so it never sees internal notes about its own ticket.
- Requester actions stay available: public replies, extra screenshots,
  confirm or reject, cancel, and reopen where policy allows it.
- An account can never be assigned a ticket it submitted. `can_assign`
  rejects that, and so does the `tickets_assignee_not_requester` constraint.
- A person can use a separate Admin account, with its own ID, to handle a
  ticket submitted from their operational account. Those two accounts may
  share an email address.
- Every role submits tickets only for its own account
  (`can_submit_ticket_for`).

## Requester rights

- A requester sees only their own tickets. That includes drafts and read-only
  history of their own AI-solved tickets. Requesters never see internal
  content.
- Replies and extra screenshots are append-only and allowed only during
  active human work. They are not allowed during the AI phase (one fixed
  input, no chat), on closed or cancelled tickets, or on AI-solved tickets.
  The original intake is frozen.
- **Only the owning requester can confirm or reject a resolution**, and only
  while the ticket is awaiting confirmation. No staff or Admin permission can
  stand in for the requester's confirmation. Rejecting a proposal is not
  reopening.
- Deciding on the AI suggestion is for the owner only, while the ticket is
  awaiting that decision. Only an explicit "Solved" marks a ticket AI-solved.
- Skipping AI as a requester needs the organization setting
  `allow_requester_skip_ai`. It applies to the draft only.
- When a non-owner tries a requester action, the response does not reveal
  whether the ticket exists. Staff who can see the ticket get 403. Everyone
  else gets 404.

## Cancellation policy

The owning requester can cancel a submitted ticket that is still open, under
the Admin policy:

| Policy                         | Effect                                                                     |
| ------------------------------ | -------------------------------------------------------------------------- |
| `anytime_while_open` (default) | Allowed in any open state, including the AI phase                          |
| `before_work_starts`           | Allowed until a human work-start event has been recorded (409 afterwards; unassigning does not reset this) |
| `disabled`                     | 403                                                                        |

Drafts, closed tickets, cancelled tickets and AI-solved records cannot be
cancelled. The reason must be non-blank and at most 1000 characters
(`validate_cancellation_reason`).

## AI-solved records are final

- Operators, Specialists and Team Leads never see AI-solved records. That
  covers lists, detail, search, counts, exports and attachments, and the
  response is always 404.
- Admins can inspect AI-solved records (and all other persisted submissions,
  but not drafts) for administration and reporting. Viewing never changes a
  record.
- No role and no setting can reopen, escalate, reply to, cancel or edit an
  AI-solved record. The database enforces this too
  (`tickets_ai_solved_final`).
- Tickets still in the AI phase (`ai_processing`, `awaiting_ai_decision`)
  and tickets cancelled before handoff are Admin-only and read-only for
  staff.

## Reopening

You can reopen a ticket only if all of these hold:

1. It is closed by a human resolution (`requester_confirmed` or
   `auto_closed_timeout`). Cancelled and AI-solved tickets never qualify.
2. The Admin setting `reopen_closed_tickets_enabled` is on. It is **off by
   default** and no role can bypass it.
3. You have the normal permission: the owning requester, or staff under the
   claimed-ticket rule (the current assignee who is still a member, the team's
   lead, or an Admin).

## Administration

Only active Admins can manage roles, invitations, teams and leadership,
categories, organization settings, AI/BYOK settings, SMTP, reporting and the
Admin submission view (`can_manage_org`, `can_view_admin_submissions`). Team
Leads get none of these.

### Directory invariants (`admin::accounts`, `admin::teams`)

| Rule | Error |
| ---- | ----- |
| The last active Admin cannot be demoted or deactivated. All active Admin rows are locked first, so when the only two Admins demote each other at the same time, exactly one change succeeds. | 409 `last_admin` |
| Admin is mutually exclusive with team membership. Promoting a current member to Admin is rejected until the membership is ended explicitly. The User role cannot keep a membership either. | 409 `team_membership_conflict` |
| An account belongs to at most one team and can only lead its own team. Adding it to a second team, or making it lead a different team, is rejected. | 409 `team_membership_conflict` |
| Each active team has exactly one lead. Removing, demoting or deactivating the sole lead is rejected. `change_role` accepts `replacement_lead_id` to hand over leadership and change the role in a single transaction. | 409 `team_lead_required` |
| A team cannot be archived while it has open tickets. Archiving ends all memberships. | 409 `team_has_open_tickets` |
| A lead must be an active account with the Team Lead role. Team names are unique among active teams. | 422 `validation_failed` (`leadAccountId`/`accountId`, `name`: `already_taken`) |

How the services behave:

- **Atomic and isolated:** creating a team together with its lead, and
  replacing a lead, are single transactions. They take a lock on the team row,
  so concurrent replacements run one after the other and the team ends up with
  exactly one lead. A failed change rolls back completely and leaves the
  previous valid state.
- **Replaced lead:** by default (`PreviousLead::KeepAsMember`) the previous
  lead stays on the team as a plain member. It keeps the Team Lead role but
  has no lead authority. `RemoveFromTeam` takes it out of the team instead.
  A role change to User or Admin that includes a replacement always removes
  it.
- **Ending a membership** (remove, or replace with removal) clears that
  account's open assignments in the team and records a `ticket.unassigned`
  ticket event.
- **Shared email:** accounts that share an email address are checked
  independently. Every check uses account IDs.
- **Audit:** every change writes an append-only `audit_events` row in the same
  transaction. The row records the actor's account ID, an action
  (`account.role_changed`, `account.deactivated`, `account.reactivated`,
  `team.created`, `team.renamed`, `team.lead_replaced`, `team.member_added`,
  `team.member_removed`, `team.archived`), the target, safe metadata and the
  request ID. Changes that do nothing write no row.
- `deactivate_account` / `reactivate_account` enforce the protections above.
  Revoking sessions on deactivation is the job of the session layer (#9/#10)
  and must happen in the same request.
