# Screenshots

First-release attachments are static PNG, JPEG, and WebP screenshots only
(`apps/api/src/screenshots.rs`). There is no general file upload. All endpoints
require a session; mutations also require `X-Requested-With: tikkit`.

## Upload

`POST /api/screenshots?filename=<original name>` takes the raw image as the
request body with `Content-Type: image/png`, `image/jpeg`, or `image/webp` and
returns `201 Screenshot` (`id`, normalized `filename`, `content_type`,
`byte_size`, `width`, `height`, `created_at`). Any signed-in account may upload.

The upload is untrusted. In order:

1. The declared type must be one of the three (`file: unsupported_type`) and the
   body non-empty (`file: required`); bodies over 10 MiB are `413 payload_too_large`.
2. The magic bytes decide the format. SVG, HTML, PDF, archives, executables,
   GIF, and anything else are `file: unsupported_type`. A valid image labelled
   with another type is `file: type_mismatch`; a filename extension of another
   type (`x.jpg` for a PNG, `x.png.exe`) is `filename: type_mismatch`.
3. Animated PNG (APNG) and animated WebP are `file: animated`.
4. Header dimensions are checked before any pixels are allocated: each side at
   most 16,384 and at most 40 megapixels (`file: dimensions_too_large`). The
   decoder also runs under matching allocation limits. PNG construction has a
   separate 16 MiB internal allocation budget, bounding compressed ICC metadata
   before any pixel allocation; profiles exceeding it are dropped by the decoder.
   WebP chunk lengths must fit the received bytes before the decoder sees them.
   Two uploads at a time may buffer, decode, or write files per API process;
   requests wait in FIFO order before buffering, and cancelled requests retain
   their capacity until their blocking work actually finishes. Body reads time
   out after 30 seconds (`400 bad_request`) so stalled uploads release capacity.
5. The image is fully decoded (`file: corrupt` on failure), the EXIF orientation
   applied, and re-encoded in the same format (JPEG at quality 90, WebP
   lossless). Only pixels and the ICC colour profile survive: EXIF/GPS, text
   chunks, XMP, comments, and trailing data are dropped. Width/height/size
   describe the stored file.
6. The filename keeps only its base name, letters, digits, space, `-_.()`
   (others become `_`), at most 100 characters, plus the canonical extension
   (`png`, `jpg`, `webp`); an empty name becomes `screenshot`.
7. An account may hold 50 unattached uploads (`file: too_many`). A per-account
   database lock serializes the count and insert across API processes.

Storage writes a temp file, fsyncs it, renames it to `SCREENSHOT_DIR/<id>`,
and fsyncs the parent directory before the metadata row commits. A failed write
is `500 internal` with no row; a failed database write leaves an unreferenced file for the expiry sweep. A lost
commit acknowledgement never causes deletion of a possibly committed file.
Rows never point at partial files. Uploads and deletions record `screenshot_uploaded` / `screenshot_deleted` audit events (ID,
type, size; no filename or bytes).

## Pending uploads, drafts, and submission

A new upload is **pending**: only its uploader can read or delete it; everyone
else, Admins included, gets `404`. Drafts and submissions reference pending
uploads in `intake.screenshot_ids` (at most 10, distinct). Saving a draft
requires each ID to be a pending upload of the same account (`422
screenshot_ids: invalid`; more than 10 is `too_many`) and restarts its expiry
clock. Submission calls `screenshots::attach` inside the submission
transaction: it binds every ID to the new ticket exactly once, or the whole
submission fails with `screenshot_ids: invalid` and creates nothing. A failed
submission leaves the uploads pending and the draft unchanged.

`DELETE /api/screenshots/{id}` removes an own pending upload immediately
(`204`). It is `409 conflict` while an editable draft of the account still
references it (save the draft without it first) and for every attached
screenshot. Attached rows are frozen by the `screenshots_frozen` trigger: no
update of their content or ticket, no delete (`23001`).

Removing a screenshot from a draft just means saving the draft without it; the
unreferenced upload then expires.

### Expiry

`screenshots::cleanup` runs at API startup and hourly. It deletes pending
uploads that no draft references and that were neither uploaded nor saved in a
draft for 24 hours, and records one `screenshots_expired` audit event with their
IDs. It also removes files in `SCREENSHOT_DIR` older than 24 hours that have no
row (a crash between write and insert) and leftover `*.tmp` files. Draft saves
lock the rows they touch, so cleanup cannot delete an upload that a concurrent
save keeps. Several API processes may run it at once.

## Reading

- `GET /api/screenshots/{id}`: metadata (`Cache-Control: private, no-store`).
- `GET /api/screenshots/{id}/preview`: the bytes inline, for `<img>`.
- `GET /api/screenshots/{id}/download`: the same bytes as an attachment.

Every request rechecks access, so stale URLs stop working when access ends.
Pending: the uploader only. Attached: exactly `authz::ticket_view` of the
ticket right now: its requester; staff on general-queue tickets; after
dispatch only current members of the destination team and Admins (previous
dispatchers, transferred-away teams, and removed members get `404`); AI-stage
and AI-solved tickets only the requester and Admins. Uploading an image grants
nothing once it is attached. Anything else is `404`, identical to a missing ID.

Responses carry the stored, validated type (never the client's),
`X-Content-Type-Options: nosniff`, `Content-Disposition: inline|attachment;
filename="…"; filename*=UTF-8''…`, `Content-Security-Policy: default-src
'none'; frame-ancestors 'none'; sandbox`, `Cache-Control: private, no-store`,
and `Cross-Origin-Resource-Policy: same-origin`. A record whose file is missing
fails with `500 internal` and is logged.

Files are never published under a public URL, including for AI providers: the
later AI integration reads authorized intake bytes through the backend.

## Later additions

Requester public replies and staff internal notes (#21) reuse the same upload
endpoint and `screenshots::attach(db, uploader, ids, ticket_id)` in their own
transaction after their own permission checks (open status, reply/note
permission, assignee/Team Lead/Admin on claimed tickets, AI-solved and
closed records accept nothing). Message visibility for internal-note images
belongs to that work; until then every attached screenshot is intake and
follows the public ticket view.

## Storage, backup, and restore

`SCREENSHOT_DIR` (see [configuration.md](configuration.md)) is a private
directory that only the API reads; the reverse proxy and web app never serve
it. In Compose it is the `screenshots-data` volume, so replacing containers or
images keeps the files. Back it up together with PostgreSQL: take the database
dump first, then copy the directory. Files newer than the dump are harmless
extras that the stray-file sweep removes after 24 hours; restoring an older
directory than the dump leaves rows whose previews fail with `500`.
