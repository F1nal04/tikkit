# Operator queues

`GET /api/queue` and `GET /api/queue/counts` (`apps/api/src/queues.rs`) list the
tickets staff may work on. Any staff role may call them (`view_general_queue`);
a User gets `403`. Ticket detail is `GET /api/tickets/{id}`
([lifecycle.md](lifecycle.md)); claiming and dispatching are separate actions.
Queue rows open the staff detail page `/queue/{id}` (intake, conversation with
internal notes, history, status work, resolution proposals, priority); a ticket
the caller submitted redirects to their requester page `/tickets/{id}`. Staff
writes use `POST /api/tickets/{id}/staff-actions` and the conversation endpoints.
Controls follow `TicketDetail.staff_access`; other members of a claimed ticket
read only.

## One visibility rule

`authz::staff_visible_sql(n)` is the SQL form of
`authz::ticket_view(actor, ticket) == Some(Staff)` ([authorization.md](authorization.md)).
Lists, counts, and any later search or export put it in their `WHERE`, binding
`Actor::visibility_binds()` at `$n..$n+2`, so nothing is filtered after fetching.
`tests/queues.rs` compares it with `ticket_view` for every status, team, and
requester, and compares each queue with direct detail access.

It means: not your own ticket (you are its requester, not its handler); never a
draft; AI stages and AI-solved records only for Admins; a ticket without a team
(general intake) for every staff role; a dispatched ticket only for current
members of its non-archived team and Admins. The dispatcher, former members, and
other teams see nothing of it.

Queues narrow this further: a queue lists only statuses that need human work
(`queued`, `in_progress`, `waiting_for_requester`, `awaiting_confirmation`), or
one status of those plus `closed` and `cancelled` via `status=`. Drafts, AI
stages, and AI-solved records are never queue items, not even for an Admin
(Admin reporting is separate); `status=` of those values is `400 invalid_query`.
A cancelled ticket that never reached a team may have ended during AI, so it is
left out of `general` and `all`. Awaiting-confirmation tickets stay visible to
their team and Admins, like any other team ticket.

## Views

`view=` (default `general`):

| View | Tickets |
| --- | --- |
| `general` | Shared intake: no team, no assignee. Every operator sees the same list. |
| `team` | The caller's team, claimed or not (unassigned work waiting for a claim included). An Admin sees every dispatched ticket, or one team with `team_id`. A non-member naming another team's `team_id` gets `404`. |
| `mine` | Assigned to the caller (Admins are never assignees, so always empty). |
| `all` | General intake plus every team. Admin only (`403` otherwise). |

A ticket is in exactly one of `general` or `team`; dispatch moves it from one to
the other without creating another ticket, and an unresolved or bypassed AI
submission enters `general` once, on its `queued` transition.

## Filters, order, pages

Filters: `status`, `category_id`, `priority_id`, `unassigned=true`. Priority is
the ticket's current effective priority (`tickets.priority_id`); the requester's
original choice stays in `intake_snapshot`. `sort`: `newest` (default),
`oldest`, `priority`; all break ties on `(created_at, id)`. `priority` follows
the catalog order Admins set for priority levels (`priorities.sort_order`,
ascending): the level listed first comes first, so Admins list the most urgent
level first; within a level, oldest first. Paging is the standard `limit`/`cursor`
([api-conventions.md](api-conventions.md)); the cursor is the last row's
`(created_at, id)`. A `sort=priority` cursor also carries that row's priority
rank (`{rank}.{cursor}`), so the next page continues from values in the cursor
and never looks up its ticket: a forged cursor naming a hidden ticket reveals
nothing, and reprioritizing the last row between pages skips no rows.
Index `tickets_open_work_idx` (migration `0011_operator_queues.sql`) serves open
work in creation order.

Rows carry reference, subject, status, category, priority, requester, team and
assignee display names, `created_at` (age), and `updated_at`.

## Counts

`/api/queue/counts` returns open-work counts `general`, `team`, `mine`, and
`all` (Admin only, else `null`), computed with the same predicate and the same
view definitions as the lists, so a count always equals the length of walking
that view's default list.
