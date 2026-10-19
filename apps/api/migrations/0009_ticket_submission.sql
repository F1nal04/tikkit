-- Incomplete drafts are private requester input, not tickets in staff queues.
-- Retained until owner deletion or submission; submission keeps only a tombstone.
CREATE TABLE ticket_drafts (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    requester_account_id  uuid NOT NULL REFERENCES accounts (id),
    intake                jsonb,
    version               bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    submitted_ticket_id   uuid UNIQUE REFERENCES tickets (id),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK ((intake IS NULL) = (submitted_ticket_id IS NOT NULL))
);
CREATE INDEX ticket_drafts_requester_idx ON ticket_drafts (requester_account_id, created_at DESC, id DESC)
    WHERE submitted_ticket_id IS NULL;

ALTER TABLE tickets ADD COLUMN intake_snapshot jsonb;
-- NULL is for records predating this migration; all new submissions freeze the
-- full intake and the catalog's IntakeSnapshot in the same transaction.
CREATE FUNCTION check_intake_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status <> 'draft' AND NEW.intake_snapshot IS DISTINCT FROM OLD.intake_snapshot THEN
        RAISE EXCEPTION 'submitted intake snapshot is immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tickets_intake_snapshot_immutable BEFORE UPDATE ON tickets
    FOR EACH ROW EXECUTE FUNCTION check_intake_snapshot();

-- The committed receipt is stable even if the ticket's status later changes.
-- Keys live as long as the ticket. Store a digest, never raw unsanitized input.
CREATE TABLE ticket_submission_keys (
    requester_account_id uuid NOT NULL REFERENCES accounts (id),
    idempotency_key      uuid NOT NULL,
    request_hash         bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    ticket_id            uuid NOT NULL UNIQUE REFERENCES tickets (id),
    submitted_status     ticket_status NOT NULL CHECK (submitted_status IN ('queued', 'ai_processing')),
    PRIMARY KEY (requester_account_id, idempotency_key)
);
CREATE TRIGGER ticket_submission_keys_append_only BEFORE UPDATE OR DELETE ON ticket_submission_keys
    FOR EACH ROW EXECUTE FUNCTION reject_mutation();
CREATE TRIGGER ticket_submission_keys_no_truncate BEFORE TRUNCATE ON ticket_submission_keys
    FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();
