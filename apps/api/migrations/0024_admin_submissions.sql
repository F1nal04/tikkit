-- Admin submission view (#40). See docs/admin-submissions.md.
--
-- `GET /api/admin/submissions` lists every submitted ticket newest first by
-- `(created_at, id)`, whatever its status; drafts are not submissions.
CREATE INDEX tickets_submitted_idx ON tickets (created_at, id) WHERE status <> 'draft';
