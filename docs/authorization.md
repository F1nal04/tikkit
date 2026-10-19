# Authorization

All authorization is in Rust (`apps/api/src/authz.rs`); the web app only hides
navigation. Roles are re-read from `accounts` on every request, so role changes
and deactivation apply to live sessions immediately.

## Role/permission matrix

`authz::allows(role, permission)` reads this table. Identifiers are stable
machine values. A unit test fails if this table and the code disagree.

| Permission | `user` | `operator` | `specialist` | `team_lead` | `admin` |
| --- | :-: | :-: | :-: | :-: | :-: |
| `submit_ticket` | yes | yes | yes | yes | yes |
| `view_general_queue` | - | yes | yes | yes | yes |
| `dispatch_ticket` | - | yes | yes | yes | yes |
| `claim_ticket` | - | yes | yes | yes | - |
| `assign_ticket` | - | - | - | yes | yes |
| `reply_as_staff` | - | yes | yes | yes | yes |
| `add_internal_note` | - | yes | yes | yes | yes |
| `change_priority` | - | yes | yes | yes | yes |
| `propose_resolution` | - | yes | yes | yes | yes |
| `manage_accounts` | - | - | - | - | yes |
| `manage_teams` | - | - | - | - | yes |
| `manage_categories` | - | - | - | - | yes |
| `manage_settings` | - | - | - | - | yes |
| `view_all_submissions` | - | - | - | - | yes |
| `view_reports` | - | - | - | - | yes |

- Specialist is a title only: identical to Operator.
- Team Lead adds `assign_ticket`, limited to the team they lead.
- Admins hold every permission except `claim_ticket`: they never join teams or
  become assignees. `assign_ticket` lets them assign other members.
- Requester actions (replies on own ticket, confirm/reject a resolution,
  cancellation, reopening) depend on ticket ownership and Admin policy, not on
  this table.
- Ticket permissions are capabilities; each ticket is further checked by the
  ticket policy below.

## Ticket policy

`authz::ticket_view(&actor, &ticket)` and `authz::authorize_ticket(&actor, &ticket, permission)`
take an `Actor` (account ID, role, current team and lead flag) and a `TicketScope`
(`requester_account_id`, `team_id`, `assignee_account_id`, `status`).

Visibility (`ticket_view`):

1. The requester always gets the requester view of their own ticket (public data
   only), in every status and whatever their role.
2. Staff never see drafts. Tickets in or finished by AI (`ai_processing`,
   `awaiting_ai_decision`, `ai_solved`) and `cancelled` tickets without a team
   (possibly cancelled during AI) are visible only to Admins.
3. A ticket in the general queue (`team_id` null) is visible to every staff role.
4. A dispatched ticket is visible only to staff who are current members of that
   team (not archived) and to Admins. The dispatcher and former members lose access.

Staff actions (`authorize_ticket`), in order:

1. Invisible ticket: `404 not_found`.
2. The ticket's requester can never act as staff on it, in any role
   (self-handling, compared by account ID only): `403 forbidden`.
3. The role must hold the permission, and the ticket must be open for human
   work (`queued`, `in_progress`, `waiting_for_requester`, `awaiting_confirmation`).
4. `claim_ticket`: an unclaimed ticket of the actor's team.
   `assign_ticket`: the team's Lead or an Admin.
   Other writes on a claimed ticket: its assignee, the team's Lead, or an Admin;
   other members are read-only. Unclaimed team tickets are writable by any
   member. General-queue tickets are dispatch-only: replies, notes, priority, and
   resolution happen after dispatch to a team, never in shared intake.

Assignment targets are checked by the schema (assignee is a member of the
ticket's team and never its requester); `authz::active_team_members` lists
eligible accounts.

## Denials

- **Capability** (the caller's role can never do this, e.g. a User calling an
  Admin endpoint): `403 forbidden`.
- **Resource the caller may not see** (another requester's ticket, another
  team's ticket, AI-solved records for operators): `404 not_found`, identical
  to a missing resource, so existence never leaks.
- **Visible resource, action denied** (read-only team member replying to a
  claimed ticket, requester acting as staff): `403 forbidden`.

## Using it in handlers

```rust
// Endpoint-wide capability:
authz::require_permission(&user, Permission::ManageTeams)?;

// Per ticket: load the scope in the same query/transaction that changes it.
let actor = authz::Actor::load(&mut *tx, &user).await?;
authz::authorize_ticket(&actor, &scope, Permission::ReplyAsStaff)?;
// Reads: match authz::ticket_view(&actor, &scope) { None => 404, Some(TicketView::Requester) => public fields, ... }
```

List, search, count, and export queries must apply the same rules in SQL, not
after fetching. Team helpers: `authz::team_of`, `authz::is_lead_of`,
`authz::active_team_members`.

## Last active Admin

Migration `0004_last_active_admin.sql` makes it impossible to demote,
deactivate, or delete the last active Admin by any path. Any `UPDATE accounts`
that changes `role` or `is_active`, and any `DELETE`, first takes a transaction-level advisory lock, so concurrent
changes run one after another; the row trigger then fails with constraint
`last_active_admin` (SQLSTATE `23514`) when no other active Admin remains. A
concurrent loser under `REPEATABLE READ` gets `40001` instead, which `ApiError`
maps to `409 conflict` (as is `40P01`). `ApiError` maps the
constraint to `409 last_active_admin` via `?`, so handlers just run the update.
