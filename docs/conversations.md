# Ticket conversations

`apps/api/src/messages.rs` implements append-only public replies and private
internal notes. Detail pages are separate work (#20 and #23). No message
editing/deletion endpoints are supported, for any role. No AI calls, jobs,
regeneration, automatic routing, or resolution acceptance happen on send.

## Endpoints

- `GET /api/tickets/{id}/messages?limit=&cursor=` returns `Page<Message>`.
- `POST /api/tickets/{id}/replies` sends a public reply.
- `POST /api/tickets/{id}/notes` sends a staff-only internal note.

Sends take `{idempotency_key, body, screenshot_ids?}` and return `201 Message`
only after commit. The UUID key identifies one logical send for that
account/ticket; retry with exactly the same input (including attachment order).
Reusing it for another body, attachment list, or endpoint is `409 conflict`.
Omitting `screenshot_ids` is equivalent to `[]`. Receipts last for the ticket's
lifetime. An identical retry can return the original message after closure or
reassignment without a new write, but still requires current read visibility
(including note visibility). A transfer or membership removal can therefore
make a retry `404`. No success is returned for a new send on a finished ticket.

Bodies are plain text, nonblank, at most 10,000 Unicode characters, without
NUL. Original whitespace and text are retained. Validation uses the existing
`required`, `too_long`, and `invalid` field codes and existing English/German
translations. `screenshot_ids` is at most 10 distinct own pending uploads;
attachment validation failures roll back the whole send, leaving uploads
pending and lifecycle state unchanged. The server derives author account,
`author_kind` (`requester` or `staff`), message `kind` (`public_reply` or
`internal_note`), timestamp, and visibility; unknown input fields are rejected.

## Reads and rendering

`Message` includes its UUID, ordering `sequence`, kind, author kind, display
name, timestamp, body, `body_html`, and screenshot IDs. Author account IDs are
available only in the staff representation; they are null for requesters.
Display names are current account display names, not an identity key.

Render `body` and `author_display_name` as escaped text (Svelte interpolation),
never raw HTML or Markdown. Alternatively, `body_html` is server-escaped text
with no tags, automatic links, or embedded attachments; preserve line breaks
with `white-space: pre-wrap`. URLs and Markdown are literal text. Retrieve
images only through the existing authorized screenshot endpoints, using IDs
returned by the API. Never interpret user-authored attachment URLs or HTML.

The conversation is oldest first by immutable database `sequence`, with
`limit` defaulting to 25 and clamped to 1–100. Its opaque cursor encodes a
sequence, unlike the default timestamp cursor. Sends lock the ticket before
allocating a sequence and hold that lock through commit, so concurrently
committed messages cannot appear behind an already paged position. Sequence
gaps are normal; they are not message counts. There is no total count.
Private rows are excluded in SQL **before** the limit and cursor are computed.
Refresh from the first page to discover new messages after the last page.

## Authorization

Every read uses the current ticket policy and a consistent database snapshot.
Conversation and screenshot metadata responses carry `Cache-Control: private,
no-store`, as do screenshot bytes, so caches cannot retain revoked access.
The submitting account always gets the requester view, even if it is Admin or
belongs to the support team. That view includes public replies only, never
private note bodies, IDs, images, events, or note counts. Other requesters get
`404`. Staff reads require the current destination team or Admin; unrelated
staff and former members get `404`. Notes preserved on a ticket returned to
general intake remain visible only to Admins until the next team dispatch.

New requester replies require ownership and `queued`, `in_progress`,
`waiting_for_requester`, or `awaiting_confirmation` status. Other states return
`409`. A requester reply in `waiting_for_requester` calls
`lifecycle::transition(RequesterResponded)` in the send transaction. A reply
while awaiting confirmation does not accept, reject, or reset the proposal.

New staff replies/notes use `authz::authorize_ticket`: general intake is
dispatch-only; unclaimed team tickets allow current members and Admins; claimed
tickets allow only the assignee, that team's Lead, or Admin. No account may
write a staff reply/note on its own ticket. Public replies from a staff
requester are always requester replies. Closed, cancelled, and AI-solved
records accept no new content; reopening human-closed tickets remains a
separate authorized lifecycle action. Author role, membership, leadership, and
activation are re-read under the lifecycle service's share locks before the
ticket lock, so revocation cannot race a committed write.

Screenshots bind both `ticket_id` and `message_id` in the same transaction via
`screenshots::attach_message`, using the same attachment implementation as
`screenshots::attach`. A composite foreign key prevents cross-ticket binding.
The existing frozen-row trigger makes that association permanent. Public
images follow ticket visibility; note images also require note visibility,
including for their uploader and for a staff requester on their own ticket.
Metadata, preview, and download all use the same visibility check.

## Events and future notification consumers

Each committed send inserts one append-only `ticket_events` row:
`public_reply_added` or `internal_note_added`, with actor, timestamp, and only
`{message_id}` as payload. No body, image IDs, or private content is copied into
notification payloads. The message itself preserves author and content.
Status transition detail queries continue to include only events with
`to_status`, so note events never enter requester ticket detail/history.

Notification/export workers are separate work. Consumers must select public
reply events for public notifications, reload the authorized message, and
recheck **current** ticket/message and recipient access at delivery time.
Never send internal-note events through requester channels, even without a
body, and never rely on a historical team or author identity to grant access.
