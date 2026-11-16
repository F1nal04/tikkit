# Requester AI step (web)

What a requester sees when AI assistance applies to a request: the disclosure
on the form, the wait, the one suggestion with its two outcomes, and the
receipts. The server rules are in [ai-decisions.md](ai-decisions.md),
[ai-attempts.md](ai-attempts.md), and [submission.md](submission.md); this page
only describes the browser. Nothing here grants anything: Rust decides the
route at submission and rechecks every decision.

Code: `apps/web/src/lib/aiStep.ts` (state logic),
`routes/(app)/tickets/[id]/AiStep.svelte` (the step),
`routes/(app)/tickets/[id]/+page.svelte` (polling),
`routes/(app)/tickets/new/+page.svelte` (disclosure and skip). Strings:
`aiStep` and `ticketForm` in `src/lib/i18n/{en,de}.ts`.

## On the form

The form loads `GET /api/ai/policy` (`intakeAi` in `aiStep.ts`).

| Policy and category | Form shows |
| --- | --- |
| `ai_assistance: false` | Nothing about AI. No skip control. |
| AI on, category not exempt | "AI assistance": subject, description, the answers to the conditional questions (such as the program), and attached screenshots are sent to the organization's configured external AI provider, which prepares one suggestion. |
| AI on, category "Skip AI automatically" | "AI assistance": this category goes straight to support; nothing is sent to the AI provider. No skip control. |
| AI on, not exempt, `requester_skip_allowed: true` | The disclosure plus an unchecked "Skip the AI suggestion" checkbox. |

The provider is not named: the policy response reveals nothing about it. A
checked skip sends `requester_skip: true`; it is never sent otherwise. If the
Admin switched skipping off after the page loaded, the submission is `403`,
nothing is submitted, the form says so, reloads the policy (the checkbox
disappears), and keeps the input. The skip is part of the idempotency
fingerprint, so changing it uses a new key.

## After submission

| Receipt status | Where the requester lands |
| --- | --- |
| `ai_processing` (or `awaiting_ai_decision`) | `/tickets/{id}`, which holds the AI step |
| `queued` | The form's receipt. With AI on it also says why there is no suggestion: the category is exempt, or the requester skipped. "Open the ticket" leads to the same receipt on `/tickets/{id}`. |

## The step on `/tickets/{id}`

`aiStep(ticket)` picks one view from `status`, `ai`, and the `submit` event's
`payload.route`. The status decides first: once a ticket left the AI stage, a
suggestion on the attempt (a late result, or the one the requester answered)
never brings the decision back.

| Ticket | View |
| --- | --- |
| `ai_processing` | **Waiting.** Says what is analyzed (description, program and other details, screenshots), that it went to the external provider, that it can take several minutes, that the page can be left, and that a failure goes to support automatically. The only button is the skip, and only when `requester_skip_allowed` is true. |
| `awaiting_ai_decision` | **Suggestion.** Labelled "AI-generated suggestion", with a note that it is unchecked, may be wrong, and may have misread screenshots. Explanation and ordered steps, then exactly two buttons: "Solved" and "Still need help". No text input, no regenerate. |
| `ai_solved` | **Final receipt.** The suggestion, the confirmation time (`ai.decided_at`), and that the record is read-only and was not sent to support. |
| Human status, `ai.decision = not_solved` | **Support receipt**, with the suggestion the requester tried. |
| Human status, `ai.state = handed_off` | **Support receipt** with the reason in words: `timeout`, `ai_failed`, `ai_unavailable`, `requester_skip`. |
| No attempt, route `category_exempt` or `requester_skip` | **Support receipt** with that reason. |
| No attempt, route `ai_disabled` | No AI section. |
| `cancelled` during an AI stage | A note that it was cancelled and not sent to support. |

Every support receipt says that the original request (same reference) was
passed on unchanged and need not be submitted again. The ticket's ordinary
cancellation (with a reason) stays below the step wherever
`requesterActions` allows it; it is not one of the step's outcomes.

### The suggestion text

The stored suggestion is model output and untrusted. It is rendered as text
nodes only (`white-space: pre-wrap`): no HTML, no markdown, no automatic links.
Its container carries `lang={ai.language}`, the language it was generated in.
It is shown as stored: changing the interface language neither translates nor
regenerates it, and when the two languages differ a note says so.

### Decisions

A decision is requested in one place (`decide` in `AiStep.svelte`) and only
from a button click. Loading, viewing, refreshing, polling, leaving, or closing
the page sends nothing. While a request runs, the step's buttons and the
ticket's other actions are disabled, so a second click does nothing.

| Answer | Page |
| --- | --- |
| `200` | Reloads; the receipt for the committed state appears. |
| `409` (another decision already committed, a stale tab) | Reloads, shows the committed state, and explains that the click changed nothing, naming `error.details.status`. |
| `403` on `skip_ai` | Says skipping is not allowed, reloads the policy (the button disappears); the wait continues. |
| No response, `5xx` | Says the decision could not be saved; the buttons stay for a retry, which is safe because a repeat of the committed decision is `200`. |

### Staying current

While the view is waiting or suggestion, the page polls
`GET /api/tickets/{id}/activity` every 3 seconds (`poll` in `src/lib/poll.ts`,
visible tabs only, at once on becoming visible or online) and reruns the load
when `version` differs from the one loaded with the page. A `4xx` reruns the
load too. There is no client timeout: the server ends the step (up to about
8.5 minutes with default limits) and the page shows whatever it decided. The
timer is cleared when the page is left and is not started for any settled
view. If a reload fails because the API is unreachable, the page shows the
unavailable state and keeps the timer it had, so the wait resumes by itself.

The state is only on the server. A refresh, a later visit from "My tickets",
or another device shows the same step.

### Accessibility

Each view starts with a heading that can take focus. When the view changes
while the page is open, focus moves to the new heading if it was on a control
of the step or nowhere, and a polite live region (`role="status"`) announces
"The AI suggestion is ready", "Your request was sent to the support team", or
"Recorded as solved". After a decision, focus goes to the heading unless a
usable button of the step still holds it. Button hints are linked with
`aria-describedby`. Buttons take the full width on narrow screens.

## Tests

- `src/lib/aiStep.test.ts`: the view for every status and attempt state, late
  results, announcements, failure sorting, and the form policy.
- `routes/(app)/tickets/[id]/ai-step.test.ts`: rendering, single request per
  click, `409`/`403`/retry, focus and live region, German receipts, polling
  start and stop.
- `e2e/ai-step.spec.ts` against the stub API (`e2e/stub-api.mjs` models the
  policy, routing, attempt results, and the three decisions; hooks
  `POST /api/test/ai`, `POST /api/test/ai-result`, `GET /api/test/ai-log`):
  AI off, mandatory AI, the skip toggle both ways, exempt and non-exempt
  categories, forged skip requests, disclosure, success, failure handoff,
  refresh and resume, abandonment, double clicks and a stale tab, late results
  after a handoff, cancellation, and German on a phone.

## Policy changes while the form is open

The form reads the AI policy when it loads. Before submitting a request for
which no disclosure was shown, it reads the policy again; if AI assistance was
switched on in the meantime, nothing is sent, the disclosure is shown and
focused, and the requester submits again. A switch in the moment between that
check and the submission is not caught by the page: Rust decides the route.
