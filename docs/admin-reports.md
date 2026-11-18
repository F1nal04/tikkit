# Admin reports and CSV export

The Reports page and API require the Admin-only `view_all_submissions`
permission. They use the same cohort and classifications as the
[Admin submission list](admin-submissions.md). Ticket details and immutable
events, including the reason, actor, and time of cancellation, remain available
from that list.

## Cohort and time

`GET /api/admin/reports` accepts optional `created_from`, `created_before`,
`category_id`, and `team_id`. Bounds are RFC 3339 instants, inclusive at the
start and exclusive at the end. Offsets are interpreted as instants. Daily
buckets, the date picker, and CSV timestamps use UTC. The page initially
selects the last 30 UTC calendar dates, including today. Its end date is
inclusive on the screen.

Each submitted ticket counts once on the date its ticket row was created
(`tickets.created_at`, set when the submission is persisted), including tickets
currently in AI, open, closed, or cancelled. Drafts are excluded. Category is
the frozen category ID and priority is the current effective priority. The
team filter uses the current destination team, including archived teams. An
unfiltered report includes tickets with no destination team. A team-filtered
report excludes general queue tickets and tickets that never reached operators.

This is a submission cohort, so the date interval filters submissions. Events
for those submissions can occur after the interval. Outcomes and assignments
are current as of the database snapshot. For example, filtering on January
submissions in March can include their February closures. Current outcomes
can change over time, while the immutable event history is retained. Counts
have one ticket per denominator unless labeled as events or samples.

## AI outcomes and rates

The attempt outcome uses `ai_records::OUTCOME_SQL` through
`submissions::summaries`. The report adds the submission route where there was
no attempt, and the same reporting bucket is available via the
`ai_disposition` Admin list filter.

- **Solved** requires the persisted requester `ai_solved` decision. A generated
  suggestion, visit to history, cancellation, or denied reopen adds no success.
- **Not solved** records the explicit requester decision that led to the shared
  queue, regardless of whether the help desk has since resolved the ticket.
- **Provider failed** identifies failures with a provider-specific outcome
  code (`invalid_credentials`, `rate_limited`, `upstream_unavailable`,
  `malformed_output`, or `oversized_output`). **Other AI failures** include
  ambiguous timeouts, guard refusals, invalidation, and orphan recovery. Both
  groups went to the shared queue without a suggestion. The split uses the
  retained attempt outcome and remains stable when usage metadata expires.
- **Category exempt**, **AI disabled or unconfigured**, and **authorized
  requester skip** are distinct recorded routes. The last also includes a
  requester skip while AI was processing. None contributes an attempt or
  success to the reporting rates. Skipping a processing session can still
  incur provider usage if a call had already started.
- **Processing** and **awaiting decision** are pending. **Abandoned** is a
  cancelled AI session without a recorded decision. They count in submission
  volume and in the displayed attempt count when an attempt exists, and are
  excluded from the completed-attempt denominator. Legacy records without a
  classifiable route or attempt are reported as **unknown**.
- **AI confirmation rate** is explicit AI confirmations divided by completed
  attempts on non-cancelled submissions. The denominator includes recorded
  solved, not-solved, and failed attempts, including provider failures. An
  orphaned handoff without an attempt row adds no attempt.
- **AI deflection rate** is explicit AI confirmations divided by all
  non-cancelled submissions in the cohort, including pending and bypassed
  submissions. Cancelled submissions are excluded from both rate denominators
  and never count as successful AI or operator resolutions. A rate with a zero
  denominator is displayed without a value.

## Help desk workload, closures, and observed times

A help desk sample must have a recorded transition to `queued`. The workload
counts those tickets by current team and assignee, with a separate open
subtotal. Awaiting requester confirmation is a subset of open. Identity uses
team and assignee IDs, so duplicate display names stay distinct. Workload
does not assign historical credit to a current assignee.

The current resolution buckets use the same `RESOLUTION_SQL` classification as
the Admin list. Help desk closures are broken down into explicit requester
confirmation and automatic timeout without confirmation. Cancellation is its
own terminal outcome. Rejected proposals count as events, never as closures.
A ticket can have several rejections and still be open or eventually closed.
Reopens count authorized `closed` transitions with event type `reopen`, and
later closures are counted separately from the first closure per ticket. A
reopened ticket appears open in the current breakdown and retains its first
closure history and observation. AI confirmations are outside the operator
statistics.

Each timing row shows the number of observed samples and their arithmetic mean.
The API returns elapsed seconds, or `null` with no samples.

- **First response** measures from first queue entry to the first public staff
  reply, request-for-information transition, or staff resolution proposal,
  whichever comes first. Requester messages, assignment, start-work actions,
  and internal notes add no response sample.
- **Queue to first closure** measures from first queue entry through the first
  completed human closure, whether requester-confirmed or timed out. It
  includes calendar time waiting on the requester, including any prior
  information waits or rejected proposals.
- **Queue to proposal** measures from first queue entry to the proposal version
  that led to the first closure. **Proposal to first closure** measures the wait
  from that proposal to closure. This split does not subtract earlier requester
  information waits. A pending or rejected proposal on its own adds no closure
  sample. Missing or out-of-order timestamps are excluded from the affected
  duration. There are no targets, breach indicators, or business calendars.

## CSV

`POST /api/admin/reports/export` accepts the same query, with both date
bounds required. The interval may span at most 366 elapsed days. The export
fails with `413` when more than 10,000 submissions match, so a successful
export always contains the full selected cohort. It is an attachment sorted
by submission time and then ticket ID. A valid empty result contains the
header.

Rows contain the ticket reference, category, current priority and team names,
stable outcome codes, and UTC lifecycle timestamps and counts. The first
closure reason and time remain available after reopening. The current closure
reason and timestamp are separate from `requester_confirmed_at`, which is
populated only for a currently requester-confirmed operator closure. AI
confirmation has its own timestamp. The export omits ticket text, AI
suggestions, cancellation reasons, user identity, secrets, and provider
diagnostics. Cells are quoted and quote-escaped, with formula-like prefixes
neutralized even when preceded by whitespace.

After generating a successful export, `reports::audit_export` appends
`report_exported` through the existing audit facility. The audit row records
the Admin account, normalized UTC bounds, filter IDs, and exported row count.
Failed or unauthorized exports create no export event.
