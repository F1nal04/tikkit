# Requester drafts and submission

All endpoints require a session; mutations also require `X-Requested-With: tikkit`.
Every role submits only for its signed-in account. Requester identity is never
accepted from a body or compared by email. Errors use the stable codes in
[api-conventions.md](api-conventions.md), already translated in English and German.

## Drafts

- `POST /api/ticket-drafts` saves a `TicketIntake` and returns `201 Draft`.
- `GET /api/ticket-drafts` lists this account's editable drafts using standard
  `limit`/`cursor` pagination; `GET /api/ticket-drafts/{id}` resumes one.
- `PATCH /api/ticket-drafts/{id}` takes `{version, intake}`. `intake` replaces
  the whole saved form; omitted fields reset to their empty defaults.
- `DELETE /api/ticket-drafts/{id}?version=N` deletes an editable owned draft.
- `POST /api/ticket-drafts/{id}/submit` takes
  `{version, idempotency_key, requester_skip?}` and submits the saved input.

A draft begins at version 1. Each edit increments it. Edit, delete, and submit
lock the row and compare the supplied version: stale versions or already
submitted drafts return `409 conflict`. Other accounts, including Admins, get
`404 not_found`. A failed submission leaves the draft's version and input
unchanged. On success it leaves the editable list; stale mutation URLs cannot
rewrite or delete the resulting ticket.

Drafts are stored separately from `tickets` and never enter staff views,
counts, search, or queues. They may omit category, priority, description, and
required conditional answers. Saved input is bounded to 200 characters for the
subject, 20,000 for the description, 100 answers, and 500 characters per draft
answer. NUL characters are invalid. Catalog validity is checked at submission,
so a renamed or archived category does not destroy unsent input.

**Retention policy:** editable drafts are retained without automatic expiry
until their owner deletes or submits them. Deletion removes saved input and
records a `draft_deleted` audit event (ID and version, no form text). Creation
and edits record `draft_created`/`draft_updated` with the same safe metadata.
Submission clears the saved input and retains an ID/version/ticket tombstone
for stale-write detection. Submitted snapshots, receipts, tombstones, and audit
records have no automatic expiry in this milestone. Backups can retain deleted
draft input under the deployment's backup retention policy; deleting a draft
does not rewrite backups. Scheduled retention/anonymization belongs to the
later retention work, not this endpoint.

## Submission

`POST /api/tickets` accepts:

```json
{
  "idempotency_key": "a8f5834c-9fe9-4c13-876f-de98cc1c1783",
  "intake": {
    "category_id": "6069f76d-571f-4b0d-83ac-8ab8c9119f76",
    "priority_id": "7f84baf8-f072-4fd2-ac97-821b5ca57417",
    "subject": "Word crashes",
    "description": "It closes on startup.",
    "answers": {},
    "screenshot_ids": []
  },
  "requester_skip": false
}
```

It returns `201 {id, reference, status}` only after commit. Submission validates
required/nonblank subject and description with the draft length limits, active
category/priority, visible required answers, choice IDs and free-text answers
through `catalog::validate_intake` **inside the transaction**. Unknown field IDs
are rejected; hidden/archived answers are discarded before active-answer
validation. Names, labels and active answers are frozen in the catalog
`IntakeSnapshot`, alongside trimmed subject/description and screenshot IDs in
`tickets.intake_snapshot`. Original priority stays in that snapshot; the
separate `tickets.priority_id` is the initial effective priority and can later
change through authorized staff work. Detail/history category names come from
the snapshot even after catalog renames. The current effective priority is
shown separately. A database trigger prevents subsequent snapshot updates.
Legacy tickets predating this endpoint may have a null snapshot.

Catalog tables are share-locked until commit to keep validation, names,
conditional applicability, and category routing coherent with concurrent Admin
edits. A stable ticket UUID/reference is inserted in the transaction before
`lifecycle::transition(..., Submit { requester_skip })` runs. This is the only
status mutation. The existing lifecycle rechecks the current account and
server policy, writes the attributable transition, and chooses the route.
With AI unavailable in this milestone, all submissions enter `queued` with
`payload.route = ai_disabled`, no team, no assignee, and zero provider calls.
This global check precedes category exemption and requester-skip handling.
When AI settings arrive, the lifecycle retains authority over mandatory AI,
category exemption, and the separate requester-skip toggle. Client policy,
status, ownership, and assignment fields are rejected as unknown fields.

Screenshots/uploads are #19. Until there is a validated, owned upload path,
`screenshot_ids` must be empty on drafts and submission. Every nonempty list
returns `422 validation_failed` (`screenshot_ids: invalid`); it cannot smuggle
another account's attachment or silently omit an image. No upload or provider
integration is implemented here. Original intake has no requester PATCH/delete
API; draft endpoints remain restricted to unsubmitted drafts. Later replies and
screenshots belong to separate append-only timeline endpoints.

## Idempotency and failures

Generate one UUID `idempotency_key` for a logical submission. Reuse the same key
and request after a lost response; generate a new key only for a new submission.
Keys are scoped to the account ID across both submission endpoints, independently
of email. A transaction advisory lock serializes concurrent attempts using the
same account/key across processes; the database primary key enforces uniqueness.

An identical retry returns `200` with the original receipt, including the status
at submission. Read `/api/tickets/{id}` for the current state. Reusing a key with
different input, a different draft/version, or the other endpoint returns
`409 conflict`. Fingerprints use canonical typed JSON (including defaults and
ordered answer keys), so whitespace/key ordering in JSON does not matter.
Submitted text itself is not whitespace-normalized before fingerprinting: send
the original request for a retry. Only its SHA-256 digest is retained in the
receipt ledger; hidden/stale form text is never stored there.

Receipts are retained for the ticket's lifetime. A request that fails validation,
transition, persistence, or commit consumes no key, creates no partial ticket or
transition, and does not mark a draft submitted. The caller retains direct-submit
input or resumes its saved draft, corrects it as needed, and retries. A successful
submission's draft cannot be submitted again under a different key. HTTP errors
never include SQL, raw input, or internal failure messages.

## Minimal requester history

`GET /api/tickets?limit=&cursor=` returns `Page<TicketSummary>`, newest first by
`(created_at, id)`, including the caller's own submitted tickets in every status
(including AI-solved final history). Drafts use their separate list. Even an
Admin gets only their own submissions here. Rows include reference, status,
subject, category name, effective priority name, timestamps, and the public
`TicketAssignmentView`; no staff IDs, internal notes, or requester identity.
`GET /api/tickets/{id}` retains the lifecycle visibility policy and now includes
the frozen intake. Full requester UI, filtering, replies and actions are #20;
staff queues are #22.
