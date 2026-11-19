# Help desk guide: Operators, Specialists and Team Leads

For people who work tickets. It covers the five roles, the shared queue, teams,
handling a ticket, resolution, priority and reopening. If you also submit
tickets, read the [requester guide](requester.md); the rules there apply to you
as a requester too.

The portal is available in English and German; this guide uses the English
labels.

## The five roles

Every account has exactly one of these roles. The names are fixed.

| Role | What it is |
| --- | --- |
| **User** | Submits and follows their own tickets. No help desk access. |
| **Operator** | The basic help desk role: sees the shared queue, dispatches tickets, claims and handles tickets of their team. |
| **Specialist** | The same permissions as an Operator, plus an expertise or field label an Admin sets (for example "Networking"). The label grants nothing extra. |
| **Team Lead** | An Operator who also assigns and manages **all tickets of the one team they explicitly lead**. Leadership is set by an Admin on the team; the role alone does not make someone a lead. |
| **Admin** | Manages the organization: accounts, teams, catalog, settings, reports, audit. Can work any team's tickets as an Admin, but never joins a team and never claims a ticket. |

Rules that apply to all of them:

- A non-Admin account belongs to **at most one team** and can lead only that
  team. Each active team has **exactly one** Team Lead. Admins never join or lead
  teams. A person who is both an Admin and a team lead uses **two accounts**
  (they may share an email); an Admin's organization-wide view is not team
  membership.
- Neither the Specialist label nor the Team Lead role bypasses team visibility,
  Admin-only boundaries, or the finality of AI-solved records.
- **You cannot handle your own tickets.** On a ticket your account submitted you
  cannot claim, be assigned, dispatch, reply as staff, write notes, change
  priority or propose a resolution, whatever your role, even as Team Lead or
  Admin. You can act as its requester. Another eligible staff account has to
  handle it.
- The restriction is **per account, not per person**. If you have a separate
  Admin account, that account can handle a ticket your operational account
  submitted. Sharing an email links nothing.

## Find your work

In the sidebar under **Help desk**, **General queue** opens the queue page. It
has tabs with open-work counts:

| Tab | Shows |
| --- | --- |
| **General queue** | New tickets waiting for a team. Every Operator, Specialist, Team Lead and Admin sees the same list. |
| **Team queue** | Your team's tickets, claimed or not. Admins see all teams (**All teams**). |
| **Assigned to me** | Tickets assigned to you. |
| **Everything** | General queue plus every team. Admins only. |

You can filter by status, category, priority and **Unassigned only**, sort by
**Newest first**, **Oldest first** or **Priority**, and search by ticket number
or at least 3 characters from the subject, description, category, requester or
messages. **Save these filters as a view** keeps a named set. The list refreshes
about every 15 seconds while the tab is visible.

Tickets still with the AI, tickets solved by AI, drafts, and tickets you
submitted yourself do not appear for you. AI-solved records are never help desk
work for any role. (Admins can read them in **Submissions**, read-only.)

### Where tickets come from

Every ticket that needs a person enters the **General queue** with no team and no
assignee. That is true for every category and priority, and for each AI
outcome: AI disabled or unavailable, a category that skips AI, a requester who
skipped it, a failure, or **Still need help**. There is no automatic routing
by category, priority or AI result, and no round-robin. People choose.

If AI ran, the ticket detail shows the suggestion the requester tried and what
they decided. It is context only.

## Send a ticket to a team (dispatch)

Any staff account (except the ticket's own requester) can dispatch a ticket from
the general queue; a team ticket can be sent on by a member while it is
unclaimed, or by its assignee, its Team Lead or an Admin.

1. Open the ticket (**General queue** tab, then the row).
2. Under **Team and assignment**, pick a **Destination team** and choose
   **Send to team**.

What happens, immediately:

- The ticket leaves the shared queue and waits **unassigned** in the destination
  team's queue. You do **not** pick an individual. A member of the team claims
  it, or the team's Team Lead (or an Admin) assigns it.
- **Only current members of the destination team, and Admins, can see it
  afterwards**, in lists, search, counts, the ticket itself, and screenshots. If
  you are not in that team, you lose access at once. You see a confirmation such
  as "Ticket #12 was sent to Desktop support. You are not a member of that team,
  so you can no longer open it." That is expected.
- The ticket keeps its reference, original submission, screenshots, messages,
  priority history, AI context and all timestamps. The move is recorded in
  **Team and assignment history** (who, when, from, to).
- Sending a team ticket to another team, or **Return to general queue**,
  clears the assignee and works the same way.
- **Conflicts:** if someone else changed the ticket since you loaded it (another
  dispatch, a claim, a reply), nothing is saved. You see "This ticket changed
  since you loaded it, or your access to it changed", the page refreshes, and
  you decide again. The first action wins.

Requesters keep seeing their ticket and see the team name and the assigned
person's name (or "General queue" and "Unassigned").

## Claim, assign and reassign

- **Claim ticket** (team members): you become the assignee of an unclaimed team
  ticket. Admins cannot claim.
- **Assign** (Team Lead of that team, or an Admin): choose a team member under
  **Assign to**. This works even over another member's claim; the previous
  assignee loses write permission at once. The target must be an active member of
  the team and not the ticket's requester.
- **Remove assignee** (Team Lead, Admin) returns the ticket to unclaimed.

Rights on a ticket in a team:

| Ticket state | Who can change it |
| --- | --- |
| Unclaimed | Any current team member, and Admins. |
| Claimed | Only the **assignee**, **their Team Lead**, and **Admins**. Other members can read but not change it. |
| General queue | Nobody replies or changes it there; it can only be sent to a team. Admins also only dispatch. |

A former assignee (unassigned, reassigned, left the team or deactivated)
immediately becomes read-only on the ticket. A team member who is not assigned
sees "This ticket is assigned to someone else. Only the assignee, the team lead,
and administrators can change it. You can read it." Requesters' own actions,
and self-claim of an unclaimed ticket, are not staff editing.

You can also work in bulk from the queue: select rows, choose
an action (**Claim**, **Start work**, **Wait for requester**, **Set priority**,
**Send to team**, **Return to general queue**, **Assign to**, **Remove assignee**)
and **Apply to selected** (up to 50 tickets). Each ticket is checked on its own;
the result lists those that did not change and why, so an Operator changes only
what they could change alone and a Team Lead only their own team's tickets.

## Work a ticket

Open a ticket you may change. The **Work** section shows the actions that fit
its status.

| Status | What you can do |
| --- | --- |
| **Queued** | **Start work** (marks it In progress). |
| **In progress** | Reply publicly, add internal notes, **Wait for requester**, propose a resolution, change priority. |
| **Waiting for requester information** | Wait for the reply. The requester's reply moves it back to In progress. You can also propose a resolution. |
| **Awaiting requester confirmation** | The requester confirms or rejects; you can replace the proposal. |
| **Closed**, **Cancelled**, **Solved by AI** | Read-only (see reopening below). |

**Public reply or internal note.** In **Reply or note**, **Send as** a public
reply (the requester sees it and its screenshots) or an internal note (**Only
support staff can see this**; never shown to requesters, in portal, email or
exports). Messages are permanent and cannot be edited or deleted. Bodies are
plain text up to 10,000 characters. You can attach PNG, JPEG or WebP
screenshots. Use **Wait for requester** after asking the requester a question.

**Propose resolution.** Write a **Resolution summary (visible to the requester)**
(required, up to 5,000 characters) and choose **Propose resolution**. You cannot
close a ticket yourself. The ticket becomes **Awaiting requester confirmation**
with a deadline.

- The requester can **Confirm resolved** (closed, confirmed) or choose **Still
  not solved**, which returns the same ticket to In progress with the same team
  and assignee. That is not reopening.
- If the requester does not respond, the ticket **closes automatically** after
  the Admin's confirmation period (default 168 hours / 7 days from the proposal,
  snapshotted when you propose). Reports record this separately from a
  requester-confirmed closure. Silence is never acceptance. A reply or viewing
  the notification does not confirm anything.
- **Replace proposal** supersedes the old one and starts a new deadline.
- Queued tickets and tickets waiting for information have no deadline and are
  never closed automatically.

**Reopening.** Reopening a closed ticket handled by the help desk is off by
default. When an Admin turns on **Allow reopening human-closed tickets**, the
requester sees **Reopen ticket**, and a staff member who may work the ticket
sees it as **Reopen ticket** too; the ticket returns to In progress. Do not
promise requesters that they can reopen; check the setting. Tickets solved by AI
can never be reopened or escalated by anyone, and neither can a ticket whose
content was removed by the retention policy.

## Change priority

The requester picks the starting priority. In **Change priority**, choose a
**New priority** and **Save priority**. Anyone allowed to work the ticket can
raise or lower it. **Priority history** lists every change with who and when;
the original choice is always kept. Requesters see only the current priority and
their original choice.

Priority affects **queue order and filters** only: **Sort by Priority** follows
the order Admins gave the levels (the first level listed is the most urgent),
oldest first within a level. There are no response or resolution deadlines, SLA
timers, breach reports, or business calendars in this release.

## Team Leads

You manage the tickets of the team you lead: see the **Team queue**, assign and
reassign members, remove assignees, and edit tickets claimed by others, all
within your team. You cannot touch other teams' tickets, and the same
self-handling rule applies to your own tickets. Creating teams, changing who
leads, and moving accounts between teams is Admin-only
([administrator guide](admin.md#teams-and-leadership)). The **My team** page in
the sidebar currently shows a placeholder; use the Team queue.

## Specialists

You have exactly the Operator permissions. The expertise label is descriptive,
set by an Admin, and appears on the account.

## Notifications

You are notified, in the portal and by email if configured, about new tickets
entering the shared queue (everyone on the help desk except the person who acted),
requester replies on tickets you work or an unclaimed team ticket, assignments to
you, rejected proposals and reopenings. Notifications never include internal
note text. Links open the portal; **replies to notification emails are not read**
and change nothing. You stop receiving notifications for a ticket as soon as you
lose access to it.

## Quick reference

| I want to… | Do this |
| --- | --- |
| See new tickets | **General queue** → **General queue** tab |
| Hand a ticket to a team | Open it → **Team and assignment** → **Send to team** |
| Take a ticket | **Team queue** → open an unclaimed one → **Claim ticket** |
| Give a ticket to someone | (Team Lead/Admin) **Assign to** |
| Ask the requester something | Public reply, then **Wait for requester** |
| Finish a ticket | **Propose resolution** and wait for the requester or the deadline |
| Raise urgency | **Change priority** |
