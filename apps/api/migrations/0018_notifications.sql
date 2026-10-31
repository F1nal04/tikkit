-- Durable notifications for ticket events (#27). See docs/notifications.md.
-- Rows are content-free: they name an event and an account. Text, language,
-- recipient address, and access are all resolved again when delivered.

ALTER TABLE accounts
    ADD COLUMN notify_email boolean NOT NULL DEFAULT true,
    ADD COLUMN notify_in_app boolean NOT NULL DEFAULT true;

CREATE TABLE notification_outbox (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- One logical notification per account; both channels share it.
    source_key   text NOT NULL,
    -- No foreign key: events are append-only, and a key would change what TRUNCATE ticket_events reports.
    event_id     bigint NOT NULL,
    account_id   uuid NOT NULL REFERENCES accounts (id),
    audience     text NOT NULL CHECK (audience IN ('requester', 'staff')),
    channel      text NOT NULL CHECK (channel IN ('email', 'in_app')),
    status       text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'suppressed', 'failed')),
    attempts     integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_error   text,
    created_at   timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    UNIQUE (source_key, channel)
);
CREATE INDEX notification_outbox_due_idx ON notification_outbox (available_at, id) WHERE status = 'pending';

-- The in-app inbox. Created in the transaction that marks the outbox row sent.
CREATE TABLE notifications (
    id         uuid PRIMARY KEY REFERENCES notification_outbox (id),
    account_id uuid NOT NULL REFERENCES accounts (id),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    read_at    timestamptz
);
CREATE INDEX notifications_account_idx ON notifications (account_id, created_at DESC, id DESC);

-- Runs inside the writer's transaction, so a rollback leaves no outbox rows and
-- every present and future event writer (including the auto-close worker) is
-- covered. This only decides who is worth trying; the worker rechecks current
-- access with the Rust ticket policy before anything is sent.
--   requester: receipts and public changes. Never internal notes (no such event is listed).
--   staff: intake of the shared queue (never AI-stage, AI-solved, or cancelled-before-triage
--          tickets), and team events for the assignee, or the whole team while unclaimed.
CREATE FUNCTION enqueue_ticket_notifications() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    t tickets;
    target uuid;
    prefix text;
    email boolean;
BEGIN
    IF NEW.event_type NOT IN ('submit', 'ai_failed', 'skip_ai', 'ai_not_solved', 'ai_solved',
        'assignment_changed', 'public_reply_added', 'request_information', 'propose_resolution',
        'accept_resolution', 'reject_resolution', 'confirmation_timeout', 'cancel', 'reopen') THEN
        RETURN NULL;
    END IF;
    SELECT * INTO t FROM tickets WHERE id = NEW.ticket_id;
    target := CASE WHEN NEW.event_type = 'assignment_changed'
                   THEN (NEW.payload ->> 'assignee_account_id')::uuid ELSE t.assignee_account_id END;
    prefix := 'ticket:' || NEW.ticket_id || ':' || NEW.event_type || ':'
        || COALESCE(NEW.payload ->> 'message_id', NEW.id::text) || ':';
    email := EXISTS (SELECT 1 FROM smtp_connection WHERE enabled);
    INSERT INTO notification_outbox (source_key, event_id, account_id, audience, channel)
    SELECT prefix || r.id, NEW.id, r.id, r.audience, c.channel
    FROM (
        SELECT a.id, 'requester' AS audience FROM accounts a
        WHERE a.id = t.requester_account_id AND a.is_active AND (
            NEW.event_type IN ('submit', 'ai_solved', 'request_information', 'propose_resolution',
                'accept_resolution', 'confirmation_timeout', 'cancel')
            OR (NEW.event_type = 'public_reply_added' AND NEW.actor_account_id IS DISTINCT FROM a.id))
        UNION ALL
        SELECT a.id, 'staff' FROM accounts a
        WHERE a.is_active AND a.role <> 'user' AND a.id <> t.requester_account_id
            AND a.id IS DISTINCT FROM NEW.actor_account_id
            AND t.status NOT IN ('draft', 'ai_processing', 'awaiting_ai_decision', 'ai_solved')
            AND CASE
                WHEN t.team_id IS NULL THEN t.status = 'queued'
                    AND NEW.event_type IN ('submit', 'ai_failed', 'skip_ai', 'ai_not_solved')
                WHEN NEW.event_type IN ('assignment_changed', 'public_reply_added', 'reject_resolution', 'reopen', 'cancel') THEN
                    EXISTS (SELECT 1 FROM team_memberships m JOIN teams tm ON tm.id = m.team_id
                            WHERE m.account_id = a.id AND m.team_id = t.team_id AND tm.archived_at IS NULL)
                    AND (target IS NULL OR a.id = target)
                ELSE false END
    ) r
    CROSS JOIN (VALUES ('in_app'), ('email')) c (channel)
    WHERE c.channel = 'in_app' OR email
    ON CONFLICT (source_key, channel) DO NOTHING;
    RETURN NULL;
END $$;
CREATE TRIGGER ticket_notification_outbox AFTER INSERT ON ticket_events
    FOR EACH ROW EXECUTE FUNCTION enqueue_ticket_notifications();
