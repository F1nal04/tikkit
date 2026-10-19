-- Validated, re-encoded screenshot images. The bytes live on the private
-- SCREENSHOT_DIR volume as `<id>`; this row is the only way to reach them.
-- Pending (ticket_id NULL) uploads are private to their uploader; draft
-- intake references them by ID. Attaching sets ticket_id once, after which
-- the row is frozen and visibility is the ticket's.
CREATE TABLE screenshots (
    id                  uuid PRIMARY KEY,
    uploader_account_id uuid NOT NULL REFERENCES accounts (id),
    ticket_id           uuid REFERENCES tickets (id),
    filename            text NOT NULL CHECK (char_length(filename) BETWEEN 5 AND 128),
    content_type        text NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg', 'image/webp')),
    byte_size           integer NOT NULL CHECK (byte_size > 0),
    width               integer NOT NULL CHECK (width > 0),
    height              integer NOT NULL CHECK (height > 0),
    created_at          timestamptz NOT NULL DEFAULT now(),
    -- Upload or last draft save referencing it; pending orphans expire from here.
    touched_at          timestamptz NOT NULL DEFAULT now(),
    attached_at         timestamptz,
    CHECK ((ticket_id IS NULL) = (attached_at IS NULL))
);
CREATE INDEX screenshots_ticket_idx ON screenshots (ticket_id) WHERE ticket_id IS NOT NULL;
CREATE INDEX screenshots_pending_idx ON screenshots (uploader_account_id, touched_at) WHERE ticket_id IS NULL;

-- Submitted screenshots cannot be replaced, moved, or deleted by normal actions.
CREATE FUNCTION check_screenshot_frozen() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.ticket_id IS NOT NULL THEN
        RAISE EXCEPTION 'attached screenshots are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    IF NEW.id <> OLD.id OR NEW.uploader_account_id <> OLD.uploader_account_id
       OR NEW.filename <> OLD.filename OR NEW.content_type <> OLD.content_type
       OR NEW.byte_size <> OLD.byte_size OR NEW.width <> OLD.width OR NEW.height <> OLD.height
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'screenshot content is immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER screenshots_frozen BEFORE UPDATE OR DELETE ON screenshots
    FOR EACH ROW EXECUTE FUNCTION check_screenshot_frozen();
