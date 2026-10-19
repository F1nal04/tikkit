-- Extend the scaffold's conversation table; messages and their receipts are append-only.
ALTER TABLE ticket_messages
    ADD COLUMN sequence bigint GENERATED ALWAYS AS IDENTITY,
    ADD COLUMN author_kind text,
    ADD COLUMN idempotency_key uuid,
    ADD COLUMN request_hash bytea;
UPDATE ticket_messages m SET author_kind = CASE WHEN m.author_account_id = t.requester_account_id
    THEN 'requester' ELSE 'staff' END FROM tickets t WHERE t.id = m.ticket_id;
ALTER TABLE ticket_messages
    ALTER COLUMN author_kind SET NOT NULL,
    ADD CONSTRAINT ticket_messages_author_kind CHECK (author_kind IN ('requester', 'staff')),
    ADD CONSTRAINT ticket_messages_note_author CHECK (kind <> 'internal_note' OR author_kind = 'staff'),
    ADD CONSTRAINT ticket_messages_body_bounds CHECK (char_length(body) BETWEEN 1 AND 10000 AND btrim(body) <> ''),
    ADD CONSTRAINT ticket_messages_receipt CHECK ((idempotency_key IS NULL) = (request_hash IS NULL)),
    ADD CONSTRAINT ticket_messages_hash_size CHECK (octet_length(request_hash) = 32),
    ADD CONSTRAINT ticket_messages_retry UNIQUE (ticket_id, author_account_id, idempotency_key);
CREATE UNIQUE INDEX ticket_messages_order_idx ON ticket_messages (ticket_id, sequence);
CREATE INDEX ticket_messages_public_order_idx ON ticket_messages (ticket_id, sequence) WHERE kind = 'public_reply';
CREATE TRIGGER ticket_messages_append_only BEFORE UPDATE OR DELETE ON ticket_messages
    FOR EACH ROW EXECUTE FUNCTION reject_mutation();
CREATE TRIGGER ticket_messages_no_truncate BEFORE TRUNCATE ON ticket_messages
    FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();

-- Bind message visibility in the same UPDATE that freezes an uploaded screenshot.
ALTER TABLE screenshots ADD COLUMN message_id uuid,
    ADD CONSTRAINT screenshots_message_ticket FOREIGN KEY (message_id, ticket_id)
        REFERENCES ticket_messages (id, ticket_id),
    ADD CONSTRAINT screenshots_message_attached CHECK (message_id IS NULL OR ticket_id IS NOT NULL);
CREATE INDEX screenshots_message_idx ON screenshots (message_id) WHERE message_id IS NOT NULL;
