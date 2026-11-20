# Admin reports and CSV export

The Reports page and API require the Admin-only `view_reports`
permission. They use the same cohort and classifications as the
[Admin submission list](admin-submissions.md). Ticket details and immutable
events, including the reason, actor, and time of cancellation, remain available
from that list.

## Cohort and time

`GET /api/admin/reports` requires `created_from` and `created_before` and
accepts optional `category_id` and `team_id`. Bounds are RFC 3339 instants,
inclusive at the start and exclusive at the end, with a maximum span of
366 elapsed days. Missing, reversed, empty, or overlong ranges return
`400 invalid_query`. Offsets are interpreted as instants. Buckets, date pickers,
and CSV timestamps use UTC. The page initially selects the last 30 UTC calendar
dates, including today. Its end date is inclusive. A single valid date selects
one day. For a range longer than 366 calendar dates, the page keeps the last
366 dates through the later bound.

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
  queue. The decision stays in the cohort even after a subsequent closure or
  cancellation.
- **Provider or adapter failure** covers rejected credentials, rate limits,
  model and vision errors, upstream unavailability, and malformed or oversized
  output. A timeout belongs here when a linked failed usage record identifies
  the guarded call. A blocked destination belongs here when a linked failed
  usage record identifies a provider redirect. Model and vision codes also cover
  adapter-side preparation failures.
- **Other AI failures** cover invalid input, locally blocked destinations,
  timeouts without provider evidence, guard refusals, worker failures, and any
  recorded adapter cancellation that handed off as a failure. A requester
  cancellation before decision has its own abandoned outcome. Usage retention
  can move a timeout or redirect into the other-failures row. Both rows still
  contribute to the completed-attempt denominator.
- **Category exempt**, **AI disabled or unconfigured**, and **authorized
  requester skip** are distinct recorded routes. The last also includes a
  requester skip while AI was processing. These rows add submission volume
  and zero report attempts or successes. Skipping a processing session can
  still incur provider usage if a call had already started.
- **AI withdrawn by policy or configuration** covers in-flight handoffs when a
  category becomes exempt, AI is disabled, configuration or credentials
  change, credentials become missing or unreadable, or the model becomes
  unsupported. Withdrawals add submission volume and are excluded from
  attempts and both rate denominators.
- **Missing attempt** covers a recorded AI failure handoff without a stored
  attempt. It adds submission volume and is outside the confirmation-rate
  denominator. Legacy records without a classifiable event, route, or attempt
  are reported as **unknown**.
- **Processing** and **awaiting decision** are pending. **Abandoned** is a
  cancelled AI session without a recorded decision. They contribute to
  submission volume and the displayed attempt count when an attempt exists.
  The completed-attempt denominator excludes these rows.
- **AI confirmation rate** is explicit AI confirmations divided by the sum of
  the **Solved**, **Not solved**, **Provider or adapter failure**, and
  **Other AI failures** rows. A later cancellation preserves a completed
  decision or failure in this denominator.
- **AI deflection rate** is explicit AI confirmations divided by the number of
  submissions after excluding cancelled submissions and policy withdrawals.
  Pending submissions and bypasses recorded at submission remain in the
  denominator. Cancellations add zero successes.

A rate with a zero denominator is displayed without a value. With a team
filter, both rates are unavailable and the API returns `null` for each
numerator and denominator. A team cohort excludes AI-solved tickets by design.
The AI outcome table continues to show the selected team's outcomes.

## Help desk workload, closures, and observed times

A help desk sample must have a recorded transition to `queued`. The workload
counts those tickets by current team and assignee, with a separate open
subtotal. Awaiting requester confirmation is a subset of open. Identity uses
team and assignee IDs, so duplicate display names stay distinct. Workload
does not assign historical credit to a current assignee.

The current resolution buckets use the same `RESOLUTION_SQL` classification as
the Admin list. Current and first help desk closures each break down into
explicit requester confirmation and automatic confirmation timeout. The first
closure reason comes from the historical closure event and survives a reopen.
Cancellation is its own terminal outcome. Rejected proposals count as events,
and a ticket can have several rejections. Reopens count authorized transitions
from `closed` with event type `reopen`. Later closures are counted separately
from the first closure per ticket. A reopened ticket appears open in the current
breakdown and retains its first closure history and observation. AI confirmations
are outside the operator statistics.

Drill-down links reproduce the current closure counts, the overall count that
entered the queue, and the queued, open, and awaiting-confirmation counts for
workload rows with a named assignee. They use the same queue-entry event plus
current team, assignee, and status filters where applicable. The submission list
has no null-assignee filter, so unassigned rows have no per-row link. First and
later closure, reopen, rejection, and duration sample counts have no direct
list filter. Their underlying lifecycle events are available in each submission's
detail.

Each timing row shows the number of observed samples and their arithmetic mean.
The API returns elapsed seconds, or `null` with no samples.

- **First staff response** measures from first queue entry to the first public staff
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
neutralized even when preceded by whitespace or quotes.

Both Admin exports use the same CSV encoder in `csv::line`.

After generating a successful export, `reports::audit_export` records
`report_exported` through `audit::record` in the export transaction. As with
`audit_exported`, the event holds the Admin actor, a `filters` object, and a
`rows` count. Report filters carry normalized UTC bounds and category and
team IDs. The audit writer attaches the request ID, which is also returned
in the response's `x-request-id` header. Failed or unauthorized exports
create no export event. See [Audit log](audit.md).
