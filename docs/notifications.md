# Notifications

`apps/api/src/notifications.rs` delivers ticket notifications by email (through
the Admin-configured SMTP connection, `mail::deliver`) and into an in-app inbox.
Tickets are handled **in the portal only**: emails link back to it, say that
replies must be posted there, and nothing in this module reads mail or changes a
ticket. A notification, a link, or a retry can never accept, reject, or close
anything.

## Outbox

Migration `0018` installs an `AFTER INSERT` trigger on `ticket_events`, so every
event writer (lifecycle transitions, assignment, replies, and the future
auto-close worker) enqueues rows in the **same transaction** as the change: a
rollback leaves nothing to send, and no writer can forget to enqueue. Rows are
content-free (event ID + account ID + channel); text, address, language,
preferences, and access are all resolved at delivery. `(source_key, channel)` is
unique, where the key is ticket + event type + recipient (replies use their
`message_id`), so a duplicate event cannot fan out twice. Email rows are only
written while an enabled SMTP connection exists.

| Event | Requester | Staff |
| --- | --- | --- |
| `submit`, `ai_failed`, `skip_ai`, `ai_not_solved` | `submit`: receipt | entry into the shared queue (no team): every active non-User account except the actor |
| `public_reply_added` | from staff only | requester's reply: assignee, or the whole team while unclaimed |
| `request_information`, `propose_resolution` | yes (proposal text states the deadline and that silence is not acceptance) | |
| `accept_resolution`, `confirmation_timeout` | closure receipt, naming the reason | |
| `reject_resolution`, `reopen` | | assignee, or the whole team while unclaimed |
| `cancel` | receipt with the reason | assignee, or the team while unclaimed (never for tickets cancelled before triage) |
| `assignment_changed` | | the new assignee; with no assignee (dispatch, unassign), the team |
| `ai_solved` | confirmation receipt | never |

Internal notes (`internal_note_added`) never enqueue anything. The actor is
never notified of their own action. Admins are notified only about shared-queue
intake. Authentication mail (invitation, recovery) keeps #28's direct,
best-effort sending.

## Worker

`notifications::worker_loop` (spawned in `main.rs`) calls `process_one` in a
loop; `poll_loop` is the shared "drain, then sleep" loop that the confirmation
timeout worker should reuse. `process_one` claims one due row with
`FOR UPDATE SKIP LOCKED` and keeps the lock until the outcome is committed, so
concurrent workers (or replicas) never handle a row twice. Each worker needs two
pool connections while it sends. On SIGTERM `main.rs` calls `stop_workers`: each
`poll_loop` returns after its current step (waited for up to 5 s), so a sent
message records its idempotency key before the pool closes.

At delivery the worker re-reads the account and the ticket (plain reads: no row
locks are held while SMTP talks, so a slow relay never stalls portal writes),
then applies the ticket policy again (`authz::ticket_view`):
a deactivated account, a former team member, or a dispatcher who lost access is
`suppressed`; so is anything on an AI-solved record other than the requester's own
`ai_solved` receipt, and any note event. Opted-out channels are suppressed too.
Recipients are accounts, never addresses: accounts sharing an email each get
their own message, which names the intended username.

Email uses `mail::deliver` with one SMTP try per row and the idempotency key
`notification:<id>`; a crash after the relay accepted a message makes the retry a
recorded no-op, never a second email. Failures retry with 30 s doubling backoff
(max 1 h) up to `MAX_ATTEMPTS` (8), then become `failed`. A failure never rolls
back portal work; the in-app channel is independent of the email channel. The
stable error code is kept in `last_error`, logged, and SMTP failures also show on
the Admin SMTP screen.

## Text

English and German templates (`ticket_text`), chosen by the recipient's
`language`. Only system text, the ticket reference, the username, the proposal
deadline, and the requester's cancellation reason appear. Subjects, replies,
notes, summaries, and images are never copied. Links are portal paths
(`/tickets/{id}` for the requester, `/queue/{id}` for staff) joined to
`PUBLIC_ORIGIN`; an expired session lands on the login page and returns to the
link, and access is checked again on arrival.

## API

- `GET /api/notifications?limit=&cursor=` newest first; `GET /api/notifications/unread`
  `{count}`; `POST /api/notifications/{id}/read` (204, idempotent). The inbox is
  filtered by the *current* ticket policy in SQL before paging, so a notification
  disappears (and its read call is `404`) once the caller loses access.
- `GET`/`PUT /api/notification-preferences` `{email, in_app}`.
- Admin: `GET /api/admin/notifications` (counts by status and failure codes) and
  `POST /api/admin/notifications/retry` (re-queues `failed` rows; audited as
  `notifications_retried`; no duplicates thanks to the idempotency key).

## Not here

SLA/escalation notifications, reply-by-email, digests, push, read-all, and the
auto-close worker itself (#55).
