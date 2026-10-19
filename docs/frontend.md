# Frontend structure

Navigation and route guards are a convenience: Rust enforces every permission
(see [authorization.md](authorization.md)).

## Layouts

- `routes/+layout.server.ts` loads the session and redirects: signed-out visitors go to
  `/login?next=<path>`, uninitialized deployments to `/setup`.
- `routes/+layout.svelte` picks the shell: `Shell` (signed in: sidebar navigation by
  role, account block with username and role, language select, sign out) or `PublicShell`
  (login, setup, invitation, password recovery). One shell serves requester, staff, and
  Admin, because roles differ in which navigation entries they see, not in layout.
- `routes/(app)/` holds every signed-in page. Its `+layout.ts` calls `canAccess` and
  answers `403` for denied paths. The `403` and `404` pages render inside the shell and
  read the same, so a hidden page's existence is not revealed.

## Adding a page

1. Create the route under `routes/(app)/…` (pages render inside `<main>`; start with an `<h1>`).
2. Add an entry to `NAV` in `src/lib/nav.ts` and its label to `nav` in both
   `src/lib/i18n/{en,de}.ts`. It shows only to roles that may open its `href`.
3. Restrict a new prefix in `GUARDS` (same file). `/admin`, `/queue`, and `/team` are
   already covered, so pages below them need no guard entry. A page that loads data should
   `await parent()` first so a denied route never fetches.

## Shared controls (`src/lib/components/`)

| Component | Use |
| --- | --- |
| `Field` | Label, hint, and API field error linked with `aria-describedby`; `bind:value` keeps input after a failed request. `options` renders a select. |
| `Alert` | `error` (announced), `success`/`info` (polite). |
| `Table` | Caption, scroll region reachable by keyboard, and empty state. |
| `Dialog` | Native modal `<dialog>`: focus trap, Escape, labelled by its title. |
| `EmptyState`, `Loading`, `StatusBadge`, `ErrorView` | Empty, loading, status (text plus stable `data-status`), and error pages. |

Browser actions use `api(locale)` from `src/lib/api.ts`, which flags an expired session
(`401 unauthorized`). `Shell` then shows a banner with a sign-in link that opens in a new
tab, so the open form and its unsent input stay on the page.

Language: `LanguageSelect` sets the `lang` cookie, saves the account preference when
signed in, and reloads only layout data, so mounted forms keep their input.

## Operator queues (`/queue`)

One page serves every queue view (`general`, `team`, `mine`, and the Admin-only
`all`), as tabs with open-work counts from `GET /api/queue/counts`. The address bar
is the whole state: `?view=&status=&category_id=&priority_id=&unassigned=true&sort=&cursor=`.
`src/lib/queue.ts` parses it (unknown values fall back to defaults) and builds the
tab, filter-reset, and pagination links; the filter form is a plain `GET`, so it
works before hydration. Pages go forward with the API's cursor ("Next page") or back
to the first page; there is no previous-page link because the cursor is one-way.
The load function keeps the page when the API is unreachable (`status` 0 or 5xx)
and shows an alert with a retry button that reruns the load; any other error is a
real answer and goes to the error page. Ages render after mount (relative to the
clock), so server and client markup agree; until then the UTC creation time shows.
Server rules, filters, and visibility: [queues.md](queues.md).

## Ticket form (`routes/(app)/tickets/new`)

One form for every category; the category only decides which conditional fields appear
(`visibleFields` in `src/lib/intake.ts`, from `GET /api/catalog`). Server rules stay
authoritative: `validate` mirrors them for convenience and uses the API's field-error keys
(`subject`, `category_id`, `answers.<field id>`), so server errors render through the same `Field`
controls.

- **Hidden answers** are kept in the browser (switching the category back restores them) but
  `toIntake` never puts them in a draft save or submission, and the form says which ones are not sent.
- **Autosave:** the form saves a draft about a second after the last change, one request at a
  time, using the draft's `version`. A `409`/`404` (changed or removed elsewhere) stops saving and
  asks the user to load the saved version or keep their own as a new draft; nothing is overwritten
  silently. Resuming a draft that names an archived category, priority, or choice empties that
  input and lists what could not be restored; the saved copy is only rewritten after the next edit.
  Leaving the page saves a pending edit and drops the timer.
- **Submission:** a draft in progress is submitted through its draft endpoint (so it leaves the
  list), otherwise through `POST /api/tickets`. That endpoint submits the saved copy, so when the
  visible form differs from it (answers resume could not restore) the visible intake is written
  first. `attemptFor` keeps one idempotency key per identical request and issues a new one when the
  input changed; the submit button and handler ignore clicks while a request runs.
- The receipt shows the reference, status, category, and the priority the requester chose; the
  full ticket view is `/tickets/{id}`.

## Requester history and detail

`/` lists only the signed-in account's own submissions, in pages of 25. The
address bar keeps `status`, `from`, `until` (inclusive UTC calendar dates), and
`cursor`; filter submission resets pagination. The API applies ownership,
status and creation bounds before its limit. Empty and unavailable states offer
clear recovery links or a retry.

`/tickets/{id}` serves the requester view: frozen intake and screenshots,
current handling team/assignee display names, public conversation (paged oldest
first), and public status history. It rejects the staff view; staff detail is a
separate workflow. All authored text (including AI suggestions) is escaped plain
text with preserved line breaks. Images use only authorized screenshot ID URLs;
there are no edit/delete controls. AI-solved history is permanently read-only.

Actions are affordances from current policy/state; Rust's lifecycle remains the
authority. Resolution decisions send the displayed proposal version, rejection
is independent of the reopen toggle, deadlines and timeout closure are explicit,
and cancellation requires a reason. After `403`/`409`, refresh the detail/policy
and announce the conflict without a success claim or clearing unsent input.
Use Refresh to discover reassignment, transfers, messages, or policy changes.
Reply retries reuse the idempotency key for identical content and screenshots;
changed input gets a new key. Uploads finish before sending. Language changes
keep mounted input. A reply that becomes unavailable is retained, disabled,
until the current lifecycle permits sending again.
