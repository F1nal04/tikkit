# Accessibility

The portal targets WCAG 2.2 A and AA: keyboard use, visible focus, names,
contrast, and announcements. Rust remains the authority for every action.
This page is what the automated tests actually check, and what a person with
a screen reader still has to walk. The checks run with the stub Playwright
suite (`bun run test:e2e`); a new page under `apps/web/src/routes` fails
`e2e/a11y.spec.ts` until that spec visits it and asserts that page's URL and heading.

## Automated scan

`apps/web/e2e/a11y.spec.ts` loads `@axe-core/playwright` and runs axe-core
with the tags `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`, and `wcag22aa`
(the 2.2 AA rule axe ships is target size). A visit counts as covered only
after the final URL path matches and the page's own `h1` (or, for a state,
its expected control) is visible. A "Not found" page does not cover the
route that was requested. The scan expects zero violations. `color-contrast`
incompletes fail the same way; there is no silent drop of incomplete results.

The scan visits every route, for every role that can open it, in English and
German, at 390×844, 768×1024, and 1280×800. The same six combinations also
scan these states:

| State | Where |
| --- | --- |
| Discard-draft dialog open | `/tickets/new` |
| Dispatch controls | Operator, `/queue/[id]` |
| Assign controls | Team Lead, `/queue/[id]` |
| Validation summary and invalid fields | Ticket form, sign-in, organization settings |
| Session-expired banner | Settings, after the session is expired |
| Retention preview, then the typed `DELETE` confirmation | `/admin/retention` |
| Restore-mode banner | `/admin` while the stub's restore mode is on |
| Queue bulk-action controls | `/queue` with the page checkbox checked |
| Accounts page after opening an editor | `/admin/accounts` |

| Route | Who |
| --- | --- |
| `/login`, `/setup`, `/forgot-password`, `/invite/[token]`, `/reset-password/[token]` | Signed out |
| `/`, `/tickets/new`, `/profile`, `/notifications` | User, Operator, Specialist, Team Lead, Admin |
| `/tickets/[id]` | User, in each AI state (processing, suggestion, solved, handed off, failed) and on a ticket with a resolution proposal. The new-ticket visit includes the conditional program field and a screenshot. |
| `/queue`, `/queue/[id]` | Operator (unclaimed dispatch and claimed reply, note, and proposal), Specialist (general queue), Team Lead (assignment), Admin |
| `/team` | Team Lead |
| `/admin`, `/admin/accounts`, `/admin/teams`, `/admin/settings`, `/admin/email`, `/admin/ai`, `/admin/ai/records`, `/admin/catalog`, `/admin/submissions`, `/admin/submissions/[id]`, `/admin/audit`, `/admin/reports`, `/admin/retention` | Admin |

The same visits assert that the page does not scroll horizontally. A separate
test repeats the queue, a queue ticket, settings, submissions, and the ticket
form at 320px, and a requester ticket. On the ticket and the queue ticket it
asserts the primary action's box is inside the viewport width, so reaching it
takes only vertical scrolling. A `.table-wrap` that is wider than its box
must be a keyboard-scrollable region (`tabindex="0"`). Text at a 32px root
size (200% of the 16px default) is checked on sign-in, the ticket form, a
requester ticket, and a queue ticket, in both languages.
`prefers-reduced-motion: reduce` is checked on every element: none may still
have an animation longer than 0.05s. The spinner is one of those elements.

There are no axe violation exclusions. `color-contrast` incompletes fail the scan,
except disabled catalog reorder buttons (`Move up` / `Move down`, and the German
labels). Those buttons use `opacity: 0.4` at either end of a list, and axe reports
the contrast as incomplete because it cannot resolve the disabled color. Enabled
reorder buttons are not in that set.

| Rule | Reason |
| --- | --- |
| — | No violation is excluded. |

## Keyboard tests

`apps/web/e2e/keyboard.spec.ts` drives every control in these flows with
`tabTo` (real Tab / Shift+Tab from the current focus) and Enter, Space, or
arrow keys. It does not call `locator.focus()` or `locator.press()`:

- Sign in, including the skip link, Shift+Tab back from the username field, and a visible outline on that field
- Submit a ticket (category with ArrowDown and typeahead, program, subject, description, priority)
- Decide an AI suggestion: Tab to Solved, and Tab to Still need help
- Operator: Tab to claim (Space), start work, the reply field, send, the summary, propose
- Requester: Tab to accept, and Tab to reject
- Admin: Tab to the organization name and to Save
- The discard-draft dialog: Tab to the opener, focus stays inside, Escape closes it, focus returns
- After a link activation, focus is on the page `h1` and `scrollY` is 0
- A failed ticket submit: the error summary is focused, its outline is not `none`, Tab lands on the first invalid-field link, and Enter moves focus to that field

`main:focus { outline: none }` applies only to the skip-link target. The
error summary uses `.alert[tabindex="-1"]:focus`, so the outline is visible
when a script moves focus there. Tabbed buttons, links, and fields use
`:focus-visible` and the test reads the computed `outline-style`.

## Live regions

| Change | Region |
| --- | --- |
| Page navigation | SvelteKit's route announcer. Link and `goto` navigation move focus to the page `h1` with `preventScroll: true`. Back and forward are left to the browser. The skip link still targets `#main`. |
| Loading indicator | `role="status"` |
| AI processing | `role="status"` on the visible status, `aria-busy="true"` on the step. The result, the handoff, and the solved receipt update one polite status (`data-testid="ai-announcement"`). |
| Decision in flight | A second polite status ("saving"), then the result status. No second request until another click. |
| Form errors | A focusable summary (`#form-errors`) with no `role="alert"`. It lists each failing field as a link to that field's id. Progress and success use `role="status"`. |
| Draft autosave | Not announced while it succeeds. A failure is one polite status, and the form retries on its own. |
| Dialog | The native modal announces itself. A polite status outside the dialog says "Dialog closed." only. |
| Queue | One polite status ("The queue was updated.") each time a poll sees a new change marker. The text is cleared and set again so a repeat is a DOM change. An unchanged poll says nothing. The test covers a second and a third change. |

## Failure recovery

`apps/web/e2e/recovery.spec.ts` uses one helper that arms an endpoint and a
failure mode. Each case checks that the typed value is still there, that the
summary shows that failure's own text (an offline failure must not still be
showing the rate-limit text), that a retry is possible, and that the retry of
unchanged input reuses its idempotency key when the form sends one and leaves
exactly one resulting message or decision.

| Action | Failures |
| --- | --- |
| Ticket submit | slow, 5xx, 429, 401, offline |
| Staff reply, note, and proposal | 5xx, 429, 401, offline |
| Requester reply, accept, and reject | 5xx, 401, offline |
| AI decision | 5xx, 429, 401, offline. After the 401 no further decision request is sent until Solved is clicked again |
| Organization settings and outbound email | settings: 5xx and 401 with a real re-login; outbound email: 5xx |

A 401 on those forms shows "Your session has expired. Please sign in again.",
keeps the typed value, and does not send another request until the next click.
Organization settings and the draft autosave test then sign in again in a
second tab and confirm the saved result. The stub drops per-session fixtures
when the session token is replaced, so the ticket, reply, and AI cases retry
on the still-valid cookie after the 401 response instead of opening a new session.

## Localization

`<html lang>` follows the UI language. A stored AI suggestion keeps the `lang`
it was generated in, including when the interface language differs. Dates and
numbers use `Intl` with that locale (English `Oct 20, 2026` and `1,234.5`,
German `20.10.2026` and `1.234,5`).

German is built with `mergeCatalog` over the English catalog when `messages`
is constructed. A missing or blank German string renders the English text on
every page that reads `messages[locale]`, not only through `lookupText`.
`src/routes/login/fallback.test.ts` renders the sign-in page with a blank
German heading and expects the English "Sign in". `messageKeys` checks that
the two catalogs have the same shape (`src/lib/i18n.test.ts`).

The app has one colour theme (the light palette in `src/styles.css`). Contrast
of that theme is part of the axe scan, including the states in the table above.

## Screen-reader pass

A person with VoiceOver or NVDA still needs to walk this flow. Playwright's
accessibility snapshots check names, roles, the suggestion's `lang`, and the
live-region text. They do not check speech, rotor order, or braille.

What a human still needs to confirm:

- Speech matches the visible text, including German compounds, and is not doubled
- The route announcer and the AI status are not spoken on top of each other
- Rotor headings and form controls land in the same order as Tab
- The suggestion's `lang` makes VoiceOver/NVDA switch voice
- High-contrast mode and 200% browser zoom (not only a larger root font)

### Checklist

1. **Sign in.** Open `/login`. Hear "Sign in", then the username and password
   fields. Tab from the top: the skip link is first and is announced as "Skip
   to main content". Activate it. Focus is on the main content. Type the
   username, Tab to the password, Enter. Hear the destination page title
   ("My tickets"). Focus is on that heading, and the top bar is still in view.
2. **Submit.** Open New ticket. Hear the heading, then Category. Choose
   Software and hear Program appear. Fill subject and description, choose a
   priority, submit. Hear "Preparing an AI suggestion" and "Analyzing your
   request…". There is no chat field and no regenerate control.
3. **Suggestion.** When the suggestion is ready, hear "The AI suggestion is
   ready…" once. The suggestion text is plain, in its own language, not the
   interface language when those differ. Exactly two buttons: "Solved" and
   "Still need help".
4. **Solved.** Activate Solved with Enter. Hear "Recorded as solved with the
   AI suggestion." The page is a receipt. Activating nothing else sends no
   second decision. If the request fails, hear the alert once, the buttons
   remain, and a second Enter is required.
5. **Still need help.** On another suggestion, activate Still need help. Hear
   "Your request was sent to the support team." The same request is not asked
   for again.
6. **Operator.** Sign in as an operator, open an unclaimed team ticket, Claim
   with Enter or Space. Hear the status change. Type a reply, send it, hear
   "Public reply sent." Type a resolution summary, Propose resolution, hear
   that the ticket is awaiting confirmation.
7. **Requester confirmation.** Open that ticket as the requester. Hear
   "Confirm resolved" and "Still not solved". Activate one. Hear the new
   status (closed, or back in progress). Silence is not treated as acceptance.
8. **Queue.** Leave the queue open while another person changes it twice.
   Hear "The queue was updated." both times.
