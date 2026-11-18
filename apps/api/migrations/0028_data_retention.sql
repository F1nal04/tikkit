-- Data retention, anonymization, and personal-data export (#43).
-- See docs/retention.md. Code: apps/api/src/retention.rs.
--
-- Nothing here deletes anything by itself: every retention period starts as
-- NULL ("keep"), and `automatic` starts false.

-- The policy. One row. A NULL period keeps that category for good.
CREATE TABLE retention_settings (
    id                  boolean PRIMARY KEY DEFAULT true CHECK (id),
    -- Unsubmitted drafts, counted from their last save.
    draft_days          integer CHECK (draft_days BETWEEN 1 AND 3650),
    -- The three ticket-bound periods count from the moment a ticket reached
    -- its final status (closed, cancelled, ai_solved). Open tickets never expire.
    screenshot_days     integer CHECK (screenshot_days BETWEEN 1 AND 3650),
    ai_content_days     integer CHECK (ai_content_days BETWEEN 1 AND 3650),
    ticket_content_days integer CHECK (ticket_content_days BETWEEN 1 AND 3650),
    -- Never below audit_retention_floor() (30 days, migration 0026).
    audit_days          integer CHECK (audit_days BETWEEN 30 AND 3650),
    -- Whether the retention worker applies the policy daily on its own.
    automatic           boolean NOT NULL DEFAULT false,
    updated_at          timestamptz NOT NULL DEFAULT now(),
    updated_by          uuid REFERENCES accounts (id)
);
INSERT INTO retention_settings DEFAULT VALUES;

-- One application of the policy. The periods and the clock (`as_of`) are
-- snapshotted, so a restart continues the same job. A worker holds the row
-- lock for each batch and adds to `counts` in the batch's transaction: a crash
-- loses the batch and its counts together.
CREATE TABLE retention_runs (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- NULL: started by the retention worker (automatic).
    requested_by        uuid REFERENCES accounts (id),
    request_id          uuid,
    state               text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'completed')),
    as_of               timestamptz NOT NULL,
    draft_days          integer,
    screenshot_days     integer,
    ai_content_days     integer,
    ticket_content_days integer,
    audit_days          integer,
    -- Counts only: drafts, screenshots, ai_content, ticket_content, audit_events.
    counts              jsonb NOT NULL DEFAULT '{}',
    created_at          timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at         timestamptz,
    CHECK ((state = 'completed') = (finished_at IS NOT NULL))
);
-- One run at a time.
CREATE UNIQUE INDEX retention_runs_one_pending ON retention_runs ((true)) WHERE state = 'pending';
CREATE INDEX retention_runs_created_idx ON retention_runs (created_at DESC, id DESC);

ALTER TABLE tickets ADD COLUMN content_purged_at timestamptz;
ALTER TABLE ai_attempts ADD COLUMN content_purged_at timestamptz;
-- An anonymized account is deactivated for good.
ALTER TABLE accounts
    ADD COLUMN anonymized_at timestamptz,
    ADD CONSTRAINT accounts_anonymized_inactive CHECK (anonymized_at IS NULL OR NOT is_active);

-- What replaces removed text. Language-neutral; the interface explains it.
CREATE FUNCTION retention_marker() RETURNS text LANGUAGE sql IMMUTABLE AS $$ SELECT '—' $$;

-- When a ticket reached the status it has now. The retention clock of a ticket
-- in a final status; preview and deletion use this one definition.
CREATE FUNCTION ticket_final_at(t tickets) RETURNS timestamptz LANGUAGE sql STABLE AS $$
    SELECT COALESCE(
        (SELECT max(e.created_at) FROM ticket_events e WHERE e.ticket_id = t.id AND e.to_status = t.status),
        t.updated_at)
$$;

-- ----------------------------------------------------------------------------
-- The narrow paths through the immutability triggers.
--
-- Submitted intake, AI-solved records, messages, ticket events, attached
-- screenshots, and AI attempts stay immutable for every normal action. Three
-- functions of this migration may remove expired *content* from a ticket in a
-- final status, and nothing else:
--
--   retention_delete_screenshots(ticket)  deletes the ticket's screenshot rows
--   retention_purge_ai(ticket)            blanks the AI input and suggestion
--   retention_purge_ticket(ticket)        blanks subject, description, answers,
--                                         message bodies, and the cancellation reason
--
-- As in migration 0026, a guard recognises them by the call stack (PG_CONTEXT):
-- the row change must come from the one statement inside that function, which a
-- session setting, a DO block, or a function of another name cannot produce.
-- Each guard then checks the row itself: only a ticket in a final status, only
-- the content columns, only to the marker. Status, provenance, identifiers,
-- timestamps, and every event stay, so reports keep adding up.
--
-- Limit (the same as 0026): the role that owns these objects can replace them.

CREATE FUNCTION retention_path(stack text, statement text, fn text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT stack ~ ('^PL/pgSQL function [a-z_]+\(\) line \d+ at GET DIAGNOSTICS\n'
                    || 'SQL statement "' || statement || ' [^\n]*"\n'
                    || 'PL/pgSQL function ' || fn || '\(uuid\) line \d+ at (SQL statement|RETURN QUERY)(\n|$)')
$$;

-- Replaces the function of migrations 0008/0023 with one more rule:
-- `content_purged_at` changes only on the retention path.
CREATE OR REPLACE FUNCTION check_ticket_update() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    IF NEW.content_purged_at IS DISTINCT FROM OLD.content_purged_at THEN
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF retention_path(stack, 'UPDATE tickets', 'retention_purge_ticket')
            AND OLD.status IN ('closed', 'cancelled', 'ai_solved')
            AND OLD.content_purged_at IS NULL
            AND (NEW.subject, NEW.description) = (retention_marker(), retention_marker())
            AND to_jsonb(NEW) - ARRAY['subject', 'description', 'intake_snapshot', 'content_purged_at']
              = to_jsonb(OLD) - ARRAY['subject', 'description', 'intake_snapshot', 'content_purged_at'] THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'ticket content is removed only by retention' USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.requester_account_id <> OLD.requester_account_id THEN
        RAISE EXCEPTION 'ticket requesters are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.status <> 'draft' AND (NEW.status = 'draft'
        OR (NEW.subject, NEW.description, NEW.category_id) IS DISTINCT FROM (OLD.subject, OLD.description, OLD.category_id)) THEN
        RAISE EXCEPTION 'submitted intake is frozen' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.status = 'ai_solved' AND NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'ai_solved tickets are final' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.status IN ('ai_solved', 'cancelled')
        AND (NEW.status, NEW.team_id, NEW.assignee_account_id) IS DISTINCT FROM (OLD.status, OLD.team_id, OLD.assignee_account_id)
        AND NOT ((NEW.status, NEW.team_id) IS NOT DISTINCT FROM (OLD.status, OLD.team_id)
                 AND NEW.assignee_account_id IS NULL) THEN
        RAISE EXCEPTION '% tickets are final', OLD.status USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;

-- Migration 0009. The snapshot changes only together with `content_purged_at`,
-- which check_ticket_update() above ties to the retention path and row rules.
CREATE OR REPLACE FUNCTION check_intake_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status <> 'draft' AND NEW.intake_snapshot IS DISTINCT FROM OLD.intake_snapshot
        AND NOT (OLD.content_purged_at IS NULL AND NEW.content_purged_at IS NOT NULL) THEN
        RAISE EXCEPTION 'submitted intake snapshot is immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;

-- Messages (0012): still append-only, except a body becoming the marker on a
-- ticket whose content retention already removed.
CREATE FUNCTION check_ticket_message_change() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF retention_path(stack, 'UPDATE ticket_messages', 'retention_purge_ticket')
            AND NEW.body = retention_marker()
            AND to_jsonb(NEW) - 'body' = to_jsonb(OLD) - 'body'
            AND EXISTS (SELECT 1 FROM tickets WHERE id = OLD.ticket_id AND content_purged_at IS NOT NULL) THEN
            RETURN NEW;
        END IF;
    END IF;
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = 'restrict_violation';
END $$;
DROP TRIGGER ticket_messages_append_only ON ticket_messages;
CREATE TRIGGER ticket_messages_append_only BEFORE UPDATE OR DELETE ON ticket_messages
    FOR EACH ROW EXECUTE FUNCTION check_ticket_message_change();

-- Ticket events (0001): still append-only, except the free-text `reason` of a
-- `cancel` event leaving its payload on such a ticket. No event is deleted.
CREATE FUNCTION check_ticket_event_change() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF retention_path(stack, 'UPDATE ticket_events', 'retention_purge_ticket')
            AND OLD.event_type = 'cancel'
            AND NEW.payload = OLD.payload - 'reason'
            AND to_jsonb(NEW) - 'payload' = to_jsonb(OLD) - 'payload'
            AND EXISTS (SELECT 1 FROM tickets WHERE id = OLD.ticket_id AND content_purged_at IS NOT NULL) THEN
            RETURN NEW;
        END IF;
    END IF;
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = 'restrict_violation';
END $$;
DROP TRIGGER ticket_events_append_only ON ticket_events;
CREATE TRIGGER ticket_events_append_only BEFORE UPDATE OR DELETE ON ticket_events
    FOR EACH ROW EXECUTE FUNCTION check_ticket_event_change();

-- Screenshots (0010): an attached row is deleted only by retention, and only
-- when its ticket is in a final status. A live ticket's screenshots cannot go.
CREATE OR REPLACE FUNCTION check_screenshot_frozen() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    IF OLD.ticket_id IS NOT NULL THEN
        IF TG_OP = 'DELETE' THEN
            GET DIAGNOSTICS stack = PG_CONTEXT;
            IF retention_path(stack, 'DELETE FROM screenshots', 'retention_delete_screenshots')
                AND EXISTS (SELECT 1 FROM tickets WHERE id = OLD.ticket_id
                            AND status IN ('closed', 'cancelled', 'ai_solved')) THEN
                RETURN OLD;
            END IF;
        END IF;
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

-- AI attempts (0022): the input and the suggestion of a finished attempt are
-- blanked only by retention. State, outcome, provenance, and times stay.
CREATE OR REPLACE FUNCTION check_ai_attempt_change() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'AI attempts are never deleted' USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.content_purged_at IS DISTINCT FROM OLD.content_purged_at THEN
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF retention_path(stack, 'UPDATE ai_attempts', 'retention_purge_ai')
            AND OLD.state <> 'processing' AND OLD.content_purged_at IS NULL
            AND NEW.input = '{}'::jsonb
            AND to_jsonb(NEW) - ARRAY['input', 'explanation', 'steps', 'content_purged_at']
              = to_jsonb(OLD) - ARRAY['input', 'explanation', 'steps', 'content_purged_at']
            AND EXISTS (SELECT 1 FROM tickets WHERE id = OLD.ticket_id
                        AND status IN ('closed', 'cancelled', 'ai_solved')) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'AI content is removed only by retention' USING ERRCODE = 'restrict_violation';
    END IF;
    IF (NEW.id, NEW.ticket_id, NEW.language, NEW.input, NEW.provider, NEW.model,
        NEW.credential_set_at, NEW.created_at, NEW.deadline_at)
       IS DISTINCT FROM
       (OLD.id, OLD.ticket_id, OLD.language, OLD.input, OLD.provider, OLD.model,
        OLD.credential_set_at, OLD.created_at, OLD.deadline_at) THEN
        RAISE EXCEPTION 'AI attempt input is immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.state <> 'processing'
       AND (NEW.state, NEW.outcome, NEW.explanation, NEW.steps, NEW.finished_at, NEW.claims)
           IS DISTINCT FROM
           (OLD.state, OLD.outcome, OLD.explanation, OLD.steps, OLD.finished_at, OLD.claims) THEN
        RAISE EXCEPTION 'finished AI attempts are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.state <> 'processing' AND NEW.usage_id IS NOT NULL
       AND NEW.usage_id IS DISTINCT FROM OLD.usage_id THEN
        RAISE EXCEPTION 'finished AI attempts are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;

-- The three functions. Each statement is on one line: the guards match it.
-- All are no-ops on a ticket that is not in a final status or already done,
-- so a retried batch does nothing twice.

CREATE FUNCTION retention_delete_screenshots(ticket uuid) RETURNS SETOF uuid
LANGUAGE plpgsql AS $$
BEGIN
    RETURN QUERY
    DELETE FROM screenshots s USING tickets t WHERE s.ticket_id = ticket AND t.id = s.ticket_id AND t.status IN ('closed', 'cancelled', 'ai_solved') RETURNING s.id;
END $$;

CREATE FUNCTION retention_purge_ai(ticket uuid) RETURNS boolean LANGUAGE plpgsql AS $$
BEGIN
    UPDATE ai_attempts a SET input = '{}'::jsonb, explanation = CASE WHEN a.explanation IS NOT NULL THEN retention_marker() END, steps = CASE WHEN a.steps IS NOT NULL THEN '[]'::jsonb END, content_purged_at = clock_timestamp() FROM tickets t WHERE a.ticket_id = ticket AND t.id = a.ticket_id AND t.status IN ('closed', 'cancelled', 'ai_solved') AND a.state <> 'processing' AND a.content_purged_at IS NULL;
    RETURN FOUND;
END $$;

CREATE FUNCTION retention_purge_ticket(ticket uuid) RETURNS boolean LANGUAGE plpgsql AS $$
BEGIN
    UPDATE tickets SET subject = retention_marker(), description = retention_marker(), intake_snapshot = CASE WHEN intake_snapshot IS NULL THEN NULL ELSE jsonb_set(jsonb_set(intake_snapshot || jsonb_build_object('subject', retention_marker(), 'description', retention_marker()), '{catalog,answers}', '[]'::jsonb), '{screenshot_ids}', '[]'::jsonb) END, content_purged_at = clock_timestamp() WHERE id = ticket AND status IN ('closed', 'cancelled', 'ai_solved') AND content_purged_at IS NULL;
    IF NOT FOUND THEN
        RETURN false;
    END IF;
    UPDATE ticket_messages SET body = retention_marker() WHERE ticket_id = ticket AND body <> retention_marker();
    UPDATE ticket_events SET payload = payload - 'reason' WHERE ticket_id = ticket AND event_type = 'cancel' AND payload ? 'reason';
    RETURN true;
END $$;

-- Candidates of a run by final status.
CREATE INDEX tickets_final_idx ON tickets (id) WHERE status IN ('closed', 'cancelled', 'ai_solved');

-- Audit resource of the retention events (`retention_settings_updated`,
-- `retention_run_started`, `retention_run_completed`): `retention`. Otherwise
-- the function of migration 0026, unchanged.
CREATE OR REPLACE FUNCTION audit_resource_of(
    event_type text, subject uuid, metadata jsonb,
    OUT resource_type text, OUT resource_id text
) LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    IF metadata ? 'ticket_id' THEN
        resource_type := 'ticket'; resource_id := metadata->>'ticket_id';
    ELSIF metadata ? 'invitation_id' THEN
        resource_type := 'invitation'; resource_id := metadata->>'invitation_id';
    ELSIF event_type LIKE 'invitation\_%' THEN
        resource_type := 'invitation';
    ELSIF metadata ? 'screenshot_id' THEN
        resource_type := 'screenshot'; resource_id := metadata->>'screenshot_id';
    ELSIF event_type LIKE 'screenshot%' THEN
        resource_type := 'screenshot';
    ELSIF metadata ? 'draft_id' THEN
        resource_type := 'draft'; resource_id := metadata->>'draft_id';
    ELSIF event_type LIKE 'catalog\_%' THEN
        resource_type := COALESCE(metadata->>'kind', 'catalog');
        resource_id := metadata#>>'{item,id}';
    ELSIF event_type IN ('team_created', 'team_updated') THEN
        resource_type := 'team';
        resource_id := COALESCE(metadata#>>'{team,id}', metadata#>>'{to,id}');
    ELSIF event_type = 'organization_settings_updated' OR event_type LIKE 'setup\_%' THEN
        resource_type := 'organization';
    ELSIF event_type LIKE 'smtp\_%' OR event_type = 'notifications_retried' THEN
        resource_type := 'smtp';
    ELSIF event_type LIKE 'ai\_credential\_%' THEN
        resource_type := 'ai_credential'; resource_id := metadata->>'provider';
    ELSIF event_type LIKE 'ai\_%' THEN
        resource_type := 'ai_settings';
    ELSIF event_type LIKE 'audit\_%' THEN
        resource_type := 'audit_log';
    ELSIF event_type LIKE 'retention\_%' THEN
        resource_type := 'retention'; resource_id := metadata->>'run_id';
    ELSIF subject IS NOT NULL THEN
        resource_type := 'account'; resource_id := subject::text;
    END IF;
    resource_id := left(resource_id, 128);
END $$;
