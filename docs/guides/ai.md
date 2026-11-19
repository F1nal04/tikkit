# AI assistance guide

Optional AI assistance proposes **one** solution to a requester before their
ticket reaches the help desk. It uses your organization's own API key (bring your
own key, BYOK). This page is for Admins who decide whether to use it and for
anyone who needs to know what it does with their data.

Key points up front:

- **AI is off by default and never required.** The service desk, queues, email and
  every other feature work without any provider key.
- **AI does not resolve tickets.** Only the requester's explicit **Solved**
  records an AI resolution. Nothing else (a suggestion appearing, a page view,
  silence, a failure) ever marks a ticket solved.
- **AI-solved records are final.** No role can reopen or escalate them.
- There is **no chat**, no follow-up question and no second suggestion.

Reference pages: [AI configuration](../ai-configuration.md),
[providers](../ai-providers.md), [usage and privacy](../ai-usage.md),
[attempts](../ai-attempts.md), [decisions](../ai-decisions.md).

## Supported providers

| Provider | Credential | Notes |
| --- | --- | --- |
| **OpenAI** | An OpenAI API key | Official OpenAI API only. |
| **Google Gemini** | A Gemini API key | Official Gemini API only. |
| **Anthropic** | An Anthropic API key | Official Anthropic API only. |

Requests go only to each provider's public HTTPS host. There is no custom
endpoint or base-URL setting, and **no self-hosted or other provider** in this
release. Additional hosted providers and self-hosted models are later scope.
Each provider uses only its own stored key; switching provider never sends a key
to another vendor.

## Set it up

You need an Admin account, a provider account with an API key (and billing, if the
provider requires it), and outbound HTTPS from the API to the provider. Open
**Administration → AI assistance**.

1. **Provider** and **Model.** Choose one provider, then a model. The selector lists
   only models this release supports **that accept image input**, because
   screenshots are sent as images. There is no free-text model field; if a later
   release drops a model you selected, the page says this version no longer supports the selected model and you
   choose another. Save the selection first; the connection test uses the saved
   selection.
2. **API key.** Under **API keys**, paste the key for the selected provider and
   choose **Save key**. Keys are:
   - encrypted before storage with the deployment's `ENCRYPTION_KEY`, bound to that
     provider,
   - **write-only**: never displayed again (only the last four characters and when
     it was set), never returned by the API, never written to logs, errors or the
     audit log.
3. **Test connection.** It checks that the key is accepted and the model exists and
   can analyze images. It generates nothing and costs no tokens. Failures show a
   stable message, never provider text: key rejected, rate limit or quota (check the
   provider's plan and billing), model unsupported or not available to this key,
   provider unreachable.
4. **Enable AI assistance.** Needs a provider, a model and a readable key. It takes
   effect for the next ticket.

The status line tells you the state: AI off, enabled but the provider has no key,
key unreadable, model unsupported, or enabled and configured. Anything other than
configured sends new tickets straight to the help desk.

**Rotate a key** (leaked or expiring): create a new key at the provider, **Replace
key**, then revoke the old key at the provider. **Remove a key**: **Remove key**.
Removing the selected provider's key also switches AI off. To stop all provider calls
at once, switch **Enable AI assistance** off.

Disabling AI, choosing another provider or model, and replacing or removing the
selected key also stop AI attempts still in progress: those tickets go to the shared
queue immediately, and a late provider result is discarded.

**If `ENCRYPTION_KEY` changes or is lost**, stored provider keys become unreadable.
Ticketing keeps working and new tickets skip AI; enter the keys again
([troubleshooting](troubleshooting.md#stored-credentials-are-unreadable-after-a-restore-or-key-change)).

## What is sent to the provider, and what is not

When AI applies to a ticket, the provider receives **only**:

- the category name, subject and description,
- the answers to the conditional questions that were visible (for example the
  program name), and
- the screenshots attached to the original submission, as real image inputs,
- plus fixed instructions and the requested language (English or German).

It does **not** receive: the requester's name or account, any other ticket, later
replies or screenshots, internal notes, hidden or stale answers, or any link. Private
image URLs are never published for the provider to fetch.

Before sending, known secret patterns (password lines, API keys, tokens, private
keys, URLs with credentials) are replaced with `[redacted]`. This is pattern
matching and has limits; it does not look inside screenshots. The requester is
told on the ticket form, before submitting, that their text and screenshots go to the
external provider. A screenshot that is too large for the provider is scaled or
re-encoded to fit; if one cannot be prepared, the AI step fails and the ticket goes to
humans. Images are never silently left out of a request that then claims to have
read them.

Tikkit stores the minimized input and the one suggestion with the ticket, and
metadata only for usage accounting (provider, model, token counts, outcome codes).
It stores no prompt text, ticket content or image data in logs, usage records or
the audit log. Knowledge-base retrieval, grounding and citations **do not exist** in this
release; the model works from the ticket alone and is told not to invent sources.
The suggestion is untrusted model text shown as plain text, in the language it was
generated in; it cannot run actions or change a ticket.

## What the requester experiences

1. The form discloses AI use (unless the category is exempt).
2. After submission the ticket shows "Preparing an AI suggestion". It usually takes
   under a minute and is bounded (about eight minutes at most with the default
   limits); the requester can leave and return.
3. One **AI-generated suggestion** appears with two buttons: **Solved** or **Still
   need help**. No text box, no regenerate. Repeating a click after a network error
   is safe and is not a second solution.
4. **Solved** ends the ticket as AI-solved (final). **Still need help** sends the same
   ticket, with its original content and the suggestion, to the shared general queue.
5. If AI cannot produce a suggestion (provider error, timeout, budget used up, circuit
   breaker open, bad key, unsupported image), the ticket goes to the shared queue
   automatically, **exactly once**, with no action needed from the requester. A
   requester can also cancel a ticket during AI; cancellation ends it and does not
   send it to the help desk.

## Mandatory AI, skipping and exemptions

Evaluated on the server for every new ticket, in this order:

| Order | Control | Where | Default | Result |
| --- | --- | --- | --- | --- |
| 1 | AI assistance disabled or unusable | AI assistance | Off | Straight to the shared queue. No provider call. Nothing below applies. |
| 2 | **Skip AI automatically** on the category | Ticket catalog | Off | Straight to the shared queue. **Zero provider calls**, whatever the next switch says. |
| 3 | Otherwise AI is **mandatory** | n/a | n/a | The requester cannot skip. |
| 4 | **Allow requesters to skip AI assistance** | Organization settings | Off | Only now may the requester skip, at submission or while the suggestion is being prepared. It never enables AI and never overrides step 1 or 2. |

Do not describe AI to requesters as always on; it applies only when you enabled it and
the category is not exempt. Changing a setting affects new tickets and stops pending
attempts as described above. All decisions and settings changes are audited
([admin guide](admin.md#ai-controls-and-their-precedence)).

## Budgets and safeguards

**AI assistance → Budgets and limits** protect cost and availability. When a limit
stops a call, the ticket goes to the help desk instead; submitting tickets is never
blocked.

| Setting | Default |
| --- | --- |
| Budget window | 24 hours (1–744) |
| Requests per window, whole organization | 500 |
| Requests per window, per requester | 10 |
| Tokens per window, whole organization / per requester | no limit |
| Calls in progress at once | 2 |
| Wait for a free slot | 30 seconds |
| Provider failures in a row before calls pause | 5 |
| Pause before the provider is tried again | 60 seconds |
| Keep usage records | 90 days |

Every admitted call counts, success or failure. Token budgets use the provider's
reported counts after a call and a deliberately high estimate before it. Tikkit knows
no prices and shows **no cost**; use your provider's billing view. The **Usage**
report on the same page shows calls, failures, refusals, screenshots sent, provider-
reported tokens and the circuit-breaker state per provider.

## Finality and statistics

An **AI-solved** record is permanently final. It stays readable to its requester,
visible to Admins in **Submissions** and **AI outcomes and records**, counted in
reports, and **never** appears in operator queues, search, counts, exports,
attachments or notifications. Nothing reopens it: not the Admin reopening toggle, not a
role, not an API call, not a job. Attempts to change it are refused and logged. The
only thing that ever removes anything from it is your data-retention policy, which
deletes stored text and images but leaves the status and statistics.

**Statistics** (Reports → AI outcomes, AI assistance → AI outcomes) separate:
solved (explicitly confirmed), not solved, skipped, failed, category exempt, AI
disabled, cancelled during AI, still pending, awaiting a decision and abandoned.
Pending and abandoned attempts are never counted as success. The confirmation rate
divides by completed attempts only; the deflection rate divides by submissions
excluding cancellations and withdrawals. Bypassed tickets add to submission volume
but not to attempts.
See [admin reports](../admin-reports.md#ai-outcomes-and-rates).
