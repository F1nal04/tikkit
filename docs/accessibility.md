# Accessibility

The portal targets WCAG 2.2 A and AA: keyboard use, visible focus, names,
contrast, and announcements. Rust remains the authority for every action.
This page is the automated coverage, the keyboard tests, and the manual
screen-reader pass. The checks run with the stub Playwright suite
(`bun run test:e2e`); a new page under `apps/web/src/routes` fails
`e2e/a11y.spec.ts` until that spec visits it.

## Automated scan

`apps/web/e2e/a11y.spec.ts` loads `@axe-core/playwright` and runs axe-core
with the tags `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`, and `wcag22aa`
(the 2.2 AA rule axe ships is target size). It expects zero violations.

The scan visits every route, for every role that can open it, in English and
German, at 390×844, 768×1024, and 1280×800:

| Route | Who |
| --- | --- |
| `/login`, `/setup`, `/forgot-password`, `/invite/[token]`, `/reset-password/[token]` | Signed out |
| `/`, `/tickets/new`, `/profile`, `/notifications` | User, Operator, Specialist, Team Lead, Admin |
| `/tickets/[id]` | User, in each AI state (processing, suggestion, solved, handed off, failed) and on a ticket with a resolution proposal. The new-ticket visit includes the conditional program field and a screenshot. |
| `/queue`, `/queue/[id]` | Operator (unclaimed dispatch and claimed reply, note, and proposal), Specialist (general queue), Team Lead (assignment), Admin |
| `/team` | Team Lead |
| `/admin`, `/admin/accounts`, `/admin/teams`, `/admin/settings`, `/admin/email`, `/admin/ai`, `/admin/ai/records`, `/admin/catalog`, `/admin/submissions`, `/admin/submissions/[id]`, `/admin/audit`, `/admin/reports`, `/admin/retention` | Admin |

The same visits assert that the page does not scroll horizontally. A separate
test repeats `/queue`, settings, submissions, and the ticket form at 320px.
Text at a 32px root size (200% of the 16px default) keeps the sign-in heading
and username field on screen in both languages. `prefers-reduced-motion:
reduce` sets the spinner's animation to `none`.

There are no axe rule exclusions. An exclusion would need a reason in
`EXCLUSIONS` in the spec and a row in the table below.

| Rule | Reason |
| --- | --- |
| — | None. |

## Keyboard tests

`apps/web/e2e/keyboard.spec.ts` uses only the keyboard (Tab, type, Enter,
Escape) for:

- Sign in, including the skip link
- Submit a ticket (category, program, subject, description, priority)
- Decide an AI suggestion: Solved, and Still need help, each once
- Operator: claim, public reply, propose resolution
- Requester: accept a resolution, and reject one
- Admin: change the organization name and save
- The discard-draft dialog: focus stays inside, Escape closes it, focus
  returns to the button that opened it

Focus rings use `:focus-visible`. The error summary (the alert with
`id="form-errors"`) uses `:focus` as well, because a script moves focus there
after a failed submit and `:focus-visible` does not apply to that. Fields keep
`aria-invalid` and `aria-describedby`. The skip-link target (`main`) does not
draw a ring; the skip link itself does, which is the visible control.

## Live regions

| Change | Region |
| --- | --- |
| Page navigation | SvelteKit's route announcer (assertive). Focus moves to `main` when the previous control has left the document. |
| Loading indicator | `role="status"` |
| AI processing | `role="status"` on the visible status, `aria-busy="true"` on the step. The result, the handoff, and the solved receipt update one polite status (`data-testid="ai-announcement"`). |
| Decision in flight | A second polite status ("saving"), then the result status. No second request until another click. |
| Form errors | `role="alert"` on the summary. Progress and success use `role="status"`. |
| Draft autosave | Not announced while it succeeds. A failure is one polite status, and the form retries on its own. |
| Dialog | One polite status outside the dialog: "Dialog opened: …" then "Dialog closed." The dialog itself is a modal. |
| Queue | One polite status ("The queue was updated.") when a poll sees a new change marker. An unchanged poll says nothing. |

## Localization

`<html lang>` follows the UI language. A stored AI suggestion keeps the `lang`
it was generated in, including when the interface language differs. Dates and
numbers use `Intl` with that locale (English `Oct 20, 2026` and `1,234.5`,
German `20.10.2026` and `1.234,5`). `lookupText` returns the English string
when the active catalog has no text for a key. `messageKeys` checks that
English and German have the same shape (`src/lib/i18n.test.ts`).

The app has one colour theme (the light palette in `src/styles.css`). Contrast
of that theme is part of the axe scan.

## Screen-reader pass

A person with VoiceOver or NVDA still needs to walk this flow. The steps below
are what that pass should hear. Playwright's accessibility snapshots in
`e2e/keyboard.spec.ts` and `e2e/a11y.spec.ts` already check the tree for the
same steps (names, roles, the suggestion's `lang`, and the live-region text).
They do not check speech, rotor order, or braille.

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
   ("My tickets").
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
   with Enter. Hear the status change. Type a reply, send it, hear "Reply
   sent." Type a resolution summary, Propose resolution, hear that the ticket
   is awaiting confirmation.
7. **Requester confirmation.** Open that ticket as the requester. Hear
   "Confirm resolved" and "Still not solved". Activate one. Hear the new
   status (closed, or back in progress). Silence is not treated as acceptance.

Verified in the accessibility tree, not by a screen reader: the names and
roles in the keyboard snapshots, `html lang`, the suggestion `lang`, the AI
status text, the dialog open/close text, the queue update text, focus return
from the dialog, and focus on the error summary.
