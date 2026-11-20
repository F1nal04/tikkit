-- Data retention, anonymization, and personal-data export (#43).
-- See docs/retention.md. Code: apps/api/src/retention.rs, personal_data.rs.
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
    -- Bumped by every save. A preview is bound to the version it was made
    -- under, so a confirmation never applies a policy nobody reviewed.
    version             bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    updated_by          uuid REFERENCES accounts (id)
);
INSERT INTO retention_settings DEFAULT VALUES;

-- One application of the policy. The periods and the clock (`as_of`) are
-- snapshotted, so a restart continues the same job. `pending` is the
-- processing state: the worker holds the row lock for each batch, and the
-- removal routines below add to `counts` in the batch's transaction, so a
-- crash loses a batch and its counts together.
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
    -- Retries while expired rows were locked by someone else, with backoff.
    attempts            integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at     timestamptz NOT NULL DEFAULT clock_timestamp(),
    created_at          timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at         timestamptz,
    CHECK ((state = 'completed') = (finished_at IS NOT NULL))
);
-- One run at a time.
CREATE UNIQUE INDEX retention_runs_one_pending ON retention_runs ((true)) WHERE state = 'pending';
CREATE INDEX retention_runs_created_idx ON retention_runs (created_at DESC, id DESC);

-- A run is the authorization for removing expired content (below), so its
-- snapshot cannot be forged after the fact: the clock is never in the future,
-- the periods and the clock never change, a completed run never changes, and
-- runs are never deleted. Starting and completing a run is audited here, in
-- the database, so no run exists without its audit record.
CREATE FUNCTION retention_runs_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'retention runs are never deleted' USING ERRCODE = 'restrict_violation';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.as_of > clock_timestamp() OR NEW.state <> 'pending' OR NEW.counts <> '{}'::jsonb THEN
            RAISE EXCEPTION 'a retention run starts pending, empty, and not in the future'
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.state = 'completed'
        OR (NEW.id, NEW.requested_by, NEW.request_id, NEW.as_of, NEW.draft_days, NEW.screenshot_days,
            NEW.ai_content_days, NEW.ticket_content_days, NEW.audit_days, NEW.created_at)
           IS DISTINCT FROM
           (OLD.id, OLD.requested_by, OLD.request_id, OLD.as_of, OLD.draft_days, OLD.screenshot_days,
            OLD.ai_content_days, OLD.ticket_content_days, OLD.audit_days, OLD.created_at) THEN
        RAISE EXCEPTION 'a retention run''s snapshot is immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER retention_runs_guard BEFORE INSERT OR UPDATE OR DELETE ON retention_runs
    FOR EACH ROW EXECUTE FUNCTION retention_runs_guard();

CREATE FUNCTION retention_runs_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE policy jsonb;
BEGIN
    policy := jsonb_build_object('draft_days', NEW.draft_days, 'screenshot_days', NEW.screenshot_days,
        'ai_content_days', NEW.ai_content_days, 'ticket_content_days', NEW.ticket_content_days,
        'audit_days', NEW.audit_days);
    IF TG_OP = 'INSERT' THEN
        INSERT INTO audit_events (event_type, actor_account_id, actor_service, metadata, request_id)
        VALUES ('retention_run_started', NEW.requested_by,
                CASE WHEN NEW.requested_by IS NULL THEN 'retention' END,
                jsonb_build_object('run_id', NEW.id, 'as_of', NEW.as_of, 'policy', policy), NEW.request_id);
    ELSIF NEW.state = 'completed' AND OLD.state <> 'completed' THEN
        INSERT INTO audit_events (event_type, actor_service, metadata, request_id)
        VALUES ('retention_run_completed', 'retention',
                jsonb_build_object('run_id', NEW.id, 'requested_by', NEW.requested_by, 'as_of', NEW.as_of,
                                   'policy', policy, 'deleted', NEW.counts), NEW.request_id);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER retention_runs_audit AFTER INSERT OR UPDATE ON retention_runs
    FOR EACH ROW EXECUTE FUNCTION retention_runs_audit();

ALTER TABLE tickets ADD COLUMN content_purged_at timestamptz;
ALTER TABLE ai_attempts ADD COLUMN content_purged_at timestamptz;
-- An anonymized account is deactivated for good. Its placeholder name lives in
-- a reserved namespace: no other account may take a name starting with
-- `anonymized-` (NOT VALID: a name chosen before this migration stays as it is;
-- anonymization picks a free placeholder).
ALTER TABLE accounts
    ADD COLUMN anonymized_at timestamptz,
    ADD CONSTRAINT accounts_anonymized_inactive CHECK (anonymized_at IS NULL OR NOT is_active),
    ADD CONSTRAINT accounts_username_reserved
        CHECK (anonymized_at IS NOT NULL OR username_normalized NOT LIKE 'anonymized-%') NOT VALID;

-- What replaces removed text. Language-neutral; the interface explains it.
CREATE FUNCTION retention_marker() RETURNS text LANGUAGE sql IMMUTABLE AS $$ SELECT '—' $$;

-- When a ticket reached the status it has now. The retention clock of a ticket
-- in a final status; preview and deletion use this one definition.
CREATE FUNCTION ticket_final_at(t tickets) RETURNS timestamptz LANGUAGE sql STABLE AS $$
    SELECT COALESCE(
        (SELECT max(e.created_at) FROM ticket_events e WHERE e.ticket_id = t.id AND e.to_status = t.status),
        t.updated_at)
$$;

-- Free text people wrote into ticket event payloads: the requester's
-- cancellation `reason` and the staff proposal `summary`. Retention replaces
-- the values with the marker; the keys, the event, and everything else stay.
CREATE FUNCTION retention_redact_payload(payload jsonb) RETURNS jsonb LANGUAGE sql IMMUTABLE AS $$
    SELECT payload || COALESCE(
        (SELECT jsonb_object_agg(k, retention_marker()) FROM unnest(ARRAY['reason', 'summary']) k WHERE payload ? k),
        '{}'::jsonb)
$$;

-- Backups (docs/retention.md#backups, docs/backup-restore.md). While a session
-- holds `pg_advisory_lock(screenshot_files_lock_key())`, the API deletes no
-- file from SCREENSHOT_DIR: every file deletion first takes this lock in
-- shared mode with `pg_try_advisory_xact_lock_shared` and is deferred to the
-- stray-file sweep if it cannot. Database work is never blocked by it.
CREATE FUNCTION screenshot_files_lock_key() RETURNS bigint LANGUAGE sql IMMUTABLE AS $$
    SELECT 7526465891200432451::bigint
$$;

-- ----------------------------------------------------------------------------
-- Removing expired content.
--
-- Submitted intake, AI-solved records, messages, ticket events, attached
-- screenshots, and AI attempts stay immutable for every normal action. Three
-- routines of this migration may remove *content* from a ticket in a final
-- status, and nothing else:
--
--   retention_delete_screenshots(ticket, run, account)  deletes the screenshot rows
--   retention_purge_ai(ticket, run, account)            blanks the AI input and suggestion
--   retention_purge_ticket(ticket, run, account)        blanks subject, description, answers,
--                                                       message bodies, and free text in events
--
-- Two things must hold for a row to change.
--
-- 1. Authorization, checked inside the routine (retention_ticket_due), under
--    the ticket's row lock and against data read after that lock was taken.
--    Exactly one of `run` and `account` is given:
--      run      a retention run that exists, is pending, has a period set for
--               this category in its snapshot, and for whose clock the ticket
--               has expired *now* (its latest move into the final status is
--               older than the period). The removal is added to the run's
--               counts, and the run is audited when it starts and completes.
--      account  the ticket's requester, already anonymized. Each removal
--               writes its own audit event.
--    Anything else is refused (insufficient_privilege), and a ticket that is
--    not, or no longer, eligible is left untouched.
--
-- 2. The guards, as in migration 0026, recognise the one statement inside the
--    routine by the call stack (PG_CONTEXT): a session setting, a DO block, or
--    a function of another name cannot produce it. Each guard then checks the
--    row itself: only a ticket in a final status, only the content columns,
--    only to the marker. Status, provenance, identifiers, timestamps, and
--    every event stay, so reports keep adding up.
--
-- Limit (the same as 0026): the role that owns these objects can replace them.

CREATE FUNCTION retention_path(stack text, statement text, fn text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT stack ~ ('^PL/pgSQL function [a-z_]+\(\) line \d+ at GET DIAGNOSTICS\n'
                    || 'SQL statement "' || statement || ' [^\n]*"\n'
                    || 'PL/pgSQL function ' || fn || '\(uuid,uuid,uuid\) line \d+ at SQL statement(\n|$)')
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

-- Ticket events (0001): still append-only, except the free text of a `cancel`
-- reason or a `propose_resolution` summary becoming the marker on such a
-- ticket. No event is deleted; type, statuses, versions, actor, and time stay.
CREATE FUNCTION check_ticket_event_change() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF retention_path(stack, 'UPDATE ticket_events', 'retention_purge_ticket')
            AND OLD.event_type IN ('cancel', 'propose_resolution')
            AND NEW.payload = retention_redact_payload(OLD.payload)
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
            IF retention_path(stack, 'WITH gone AS \(DELETE FROM screenshots', 'retention_delete_screenshots')
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

-- The authorization of one removal (see the header above). Locks the ticket
-- and decides against what is committed now: a ticket that was reopened and
-- closed again after the worker picked it is judged by its newest closure.
-- A ticket another transaction holds is skipped (false); the run retries it.
CREATE FUNCTION retention_ticket_due(ticket uuid, category text, run uuid, account uuid)
RETURNS boolean LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    t tickets;
    days integer;
    clock timestamptz;
BEGIN
    IF (run IS NULL) = (account IS NULL) THEN
        RAISE EXCEPTION 'content is removed for a retention run or an anonymized account'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF run IS NOT NULL THEN
        SELECT CASE category
                   WHEN 'screenshots' THEN LEAST(r.screenshot_days, r.ticket_content_days)
                   WHEN 'ai_content' THEN LEAST(r.ai_content_days, r.ticket_content_days)
                   WHEN 'ticket_content' THEN r.ticket_content_days END, r.as_of
          INTO days, clock
          FROM retention_runs r WHERE r.id = run AND r.state = 'pending';
        IF NOT FOUND THEN
            RAISE EXCEPTION 'no pending retention run %', run USING ERRCODE = 'insufficient_privilege';
        END IF;
        IF days IS NULL THEN
            RAISE EXCEPTION 'retention run % has no period for %', run, category
                USING ERRCODE = 'insufficient_privilege';
        END IF;
    ELSIF NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = account AND a.anonymized_at IS NOT NULL) THEN
        RAISE EXCEPTION 'account % is not anonymized', account USING ERRCODE = 'insufficient_privilege';
    END IF;
    SELECT * INTO t FROM tickets WHERE id = ticket FOR UPDATE SKIP LOCKED;
    IF NOT FOUND OR t.status NOT IN ('closed', 'cancelled', 'ai_solved') THEN
        RETURN false;
    END IF;
    IF account IS NOT NULL THEN
        IF t.requester_account_id <> account THEN
            RAISE EXCEPTION 'ticket % was not submitted by account %', ticket, account
                USING ERRCODE = 'insufficient_privilege';
        END IF;
        RETURN true;
    END IF;
    -- A new statement: it sees every event committed before the lock was granted.
    RETURN ticket_final_at(t) < clock - make_interval(days => days);
END $$;

-- Records one removal: on the run's counts, or as the account path's audit event.
CREATE FUNCTION retention_done(run uuid, account uuid, ticket uuid, category text, n bigint)
RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF n <= 0 THEN
        RETURN;
    END IF;
    IF run IS NOT NULL THEN
        UPDATE retention_runs SET counts = counts || jsonb_build_object(category, COALESCE((counts->>category)::bigint, 0) + n) WHERE id = run;
    ELSE
        INSERT INTO audit_events (event_type, actor_service, subject_account_id, metadata)
        VALUES ('retention_account_content_removed', 'retention', account,
                jsonb_build_object('ticket_id', ticket, 'category', category, 'rows', n));
    END IF;
END $$;

-- The three routines. Each data statement is on one line: the guards match it.
-- All are no-ops on a ticket that is not eligible or already done, so a
-- retried batch does nothing twice.

CREATE FUNCTION retention_delete_screenshots(ticket uuid, run uuid, account uuid) RETURNS SETOF uuid
LANGUAGE plpgsql VOLATILE AS $$
DECLARE ids uuid[];
BEGIN
    IF NOT retention_ticket_due(ticket, 'screenshots', run, account) THEN
        RETURN;
    END IF;
    WITH gone AS (DELETE FROM screenshots s WHERE s.ticket_id = ticket RETURNING s.id) SELECT COALESCE(array_agg(id), '{}') INTO ids FROM gone;
    PERFORM retention_done(run, account, ticket, 'screenshots', cardinality(ids));
    RETURN QUERY SELECT unnest(ids);
END $$;

-- Returns whether an attempt was blanked.
CREATE FUNCTION retention_purge_ai(ticket uuid, run uuid, account uuid) RETURNS boolean
LANGUAGE plpgsql VOLATILE AS $$
BEGIN
    IF NOT retention_ticket_due(ticket, 'ai_content', run, account) THEN
        RETURN false;
    END IF;
    UPDATE ai_attempts a SET input = '{}'::jsonb, explanation = CASE WHEN a.explanation IS NOT NULL THEN retention_marker() END, steps = CASE WHEN a.steps IS NOT NULL THEN '[]'::jsonb END, content_purged_at = clock_timestamp() WHERE a.ticket_id = ticket AND a.state <> 'processing' AND a.content_purged_at IS NULL;
    IF NOT FOUND THEN
        RETURN false;
    END IF;
    PERFORM retention_done(run, account, ticket, 'ai_content', 1);
    RETURN true;
END $$;

-- Returns the number of rows changed (ticket, messages, events); 0: nothing.
CREATE FUNCTION retention_purge_ticket(ticket uuid, run uuid, account uuid) RETURNS integer
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    n integer;
    total integer := 1;
BEGIN
    IF NOT retention_ticket_due(ticket, 'ticket_content', run, account) THEN
        RETURN 0;
    END IF;
    UPDATE tickets SET subject = retention_marker(), description = retention_marker(), intake_snapshot = CASE WHEN intake_snapshot IS NULL THEN NULL ELSE jsonb_set(jsonb_set(intake_snapshot || jsonb_build_object('subject', retention_marker(), 'description', retention_marker()), '{catalog,answers}', '[]'::jsonb), '{screenshot_ids}', '[]'::jsonb) END, content_purged_at = clock_timestamp() WHERE id = ticket AND content_purged_at IS NULL;
    IF NOT FOUND THEN
        RETURN 0;
    END IF;
    UPDATE ticket_messages SET body = retention_marker() WHERE ticket_id = ticket AND body <> retention_marker();
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;
    UPDATE ticket_events SET payload = retention_redact_payload(payload) WHERE ticket_id = ticket AND event_type IN ('cancel', 'propose_resolution') AND payload <> retention_redact_payload(payload);
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;
    -- The copy of the cancellation reason in the audit mirror.
    PERFORM audit_redact_ticket_text(ticket);
    PERFORM retention_done(run, account, ticket, 'ticket_content', 1);
    RETURN total;
END $$;

-- Candidates of a run by final status.
CREATE INDEX tickets_final_idx ON tickets (id) WHERE status IN ('closed', 'cancelled', 'ai_solved');

-- ----------------------------------------------------------------------------
-- The audit log (migration 0026) with what retention needs from it. Still
-- append-only; still no setting or flag. Three changes:
--
--   * `audit_retention_purge` takes a row limit, so a long history is deleted
--     in bounded transactions. Same rules: an active Admin or the retention
--     service, the 30-day floor, purge records are never deleted, one purge
--     record per call with its count.
--   * `audit_anonymize_account` also removes the free text an account's
--     events can carry about it: the Specialist `expertise` before and after
--     an account update.
--   * A third guarded path, `audit_redact_ticket_text(ticket)`: the one piece
--     of ticket text the log holds, the requester's cancellation `reason` on
--     `ticket_cancel`, becomes the marker once retention removed that
--     ticket's content. Nothing else about the event changes, and the call
--     appends `audit_ticket_text_redacted`.

CREATE OR REPLACE FUNCTION audit_events_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    GET DIAGNOSTICS stack = PG_CONTEXT;
    IF TG_OP = 'DELETE'
        AND stack ~ ('^PL/pgSQL function audit_events_guard\(\) line \d+ at GET DIAGNOSTICS\n'
                     || 'SQL statement "DELETE FROM audit_events [^\n]*"\n'
                     || 'PL/pgSQL function audit_retention_purge\(timestamp with time zone,uuid,integer\) line \d+ at SQL statement(\n|$)')
        AND OLD.event_type <> 'audit_retention_purged'
        AND OLD.created_at < now() - audit_retention_floor() THEN
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE'
        AND stack ~ ('^PL/pgSQL function audit_events_guard\(\) line \d+ at GET DIAGNOSTICS\n'
                     || 'SQL statement "UPDATE audit_events [^\n]*"\n'
                     || 'PL/pgSQL function audit_anonymize_account\(uuid,uuid\) line \d+ at SQL statement(\n|$)')
        -- Only the username snapshots may become NULL; only `ip` and, for
        -- events about the account, its expertise text may go.
        AND (NEW.actor_username IS NULL OR NEW.actor_username IS NOT DISTINCT FROM OLD.actor_username)
        AND (NEW.subject_username IS NULL OR NEW.subject_username IS NOT DISTINCT FROM OLD.subject_username)
        AND NEW.metadata IN (OLD.metadata, OLD.metadata - 'ip',
                             OLD.metadata - ARRAY['ip', 'from_expertise', 'to_expertise'])
        AND (NEW.id, NEW.event_type, NEW.created_at) = (OLD.id, OLD.event_type, OLD.created_at)
        AND (NEW.actor_account_id, NEW.actor_role, NEW.actor_team_id, NEW.actor_service,
             NEW.subject_account_id, NEW.request_id, NEW.resource_type, NEW.resource_id, NEW.ticket_id)
            IS NOT DISTINCT FROM
            (OLD.actor_account_id, OLD.actor_role, OLD.actor_team_id, OLD.actor_service,
             OLD.subject_account_id, OLD.request_id, OLD.resource_type, OLD.resource_id, OLD.ticket_id) THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE'
        AND stack ~ ('^PL/pgSQL function audit_events_guard\(\) line \d+ at GET DIAGNOSTICS\n'
                     || 'SQL statement "UPDATE audit_events [^\n]*"\n'
                     || 'PL/pgSQL function audit_redact_ticket_text\(uuid\) line \d+ at SQL statement(\n|$)')
        -- Only the cancellation reason, only to the marker, only for a ticket
        -- whose content retention has removed.
        AND OLD.event_type = 'ticket_cancel' AND OLD.metadata ? 'reason'
        AND NEW.metadata = jsonb_set(OLD.metadata, '{reason}', to_jsonb(retention_marker()))
        AND to_jsonb(NEW) - 'metadata' = to_jsonb(OLD) - 'metadata'
        AND EXISTS (SELECT 1 FROM tickets WHERE id = OLD.ticket_id AND content_purged_at IS NOT NULL) THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only' USING ERRCODE = 'restrict_violation';
END $$;

-- At most `max_rows` events per call, oldest first (NULL: all). A batched call
-- that finds nothing writes no purge record.
CREATE FUNCTION audit_retention_purge(cutoff timestamptz, admin uuid, max_rows integer) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE removed bigint;
BEGIN
    PERFORM audit_require_admin(admin);
    IF cutoff IS NULL OR cutoff > now() - audit_retention_floor() THEN
        RAISE EXCEPTION 'audit retention cutoff must be at least % in the past', audit_retention_floor()
            USING ERRCODE = 'check_violation';
    END IF;
    IF max_rows IS NOT NULL AND max_rows < 1 THEN
        RAISE EXCEPTION 'the row limit must be positive' USING ERRCODE = 'check_violation';
    END IF;
    DELETE FROM audit_events WHERE id IN (SELECT e.id FROM audit_events e WHERE e.created_at < cutoff AND e.event_type <> 'audit_retention_purged' ORDER BY e.id LIMIT max_rows);
    GET DIAGNOSTICS removed = ROW_COUNT;
    IF removed > 0 OR max_rows IS NULL THEN
        INSERT INTO audit_events (event_type, actor_account_id, actor_service, metadata)
        VALUES ('audit_retention_purged', admin, CASE WHEN admin IS NULL THEN 'retention' END,
                jsonb_build_object('cutoff', cutoff, 'deleted', removed));
    END IF;
    RETURN removed;
END $$;

-- The unbatched form of migration 0026, now a call of the one above.
CREATE OR REPLACE FUNCTION audit_retention_purge(cutoff timestamptz, admin uuid) RETURNS bigint
LANGUAGE plpgsql AS $$
BEGIN
    RETURN audit_retention_purge(cutoff, admin, NULL);
END $$;

CREATE OR REPLACE FUNCTION audit_anonymize_account(account uuid, admin uuid) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE changed bigint;
BEGIN
    PERFORM audit_require_admin(admin);
    IF account IS NULL OR NOT EXISTS (SELECT 1 FROM accounts WHERE id = account) THEN
        RAISE EXCEPTION 'unknown account' USING ERRCODE = 'check_violation';
    END IF;
    UPDATE audit_events SET actor_username = CASE WHEN actor_account_id = account THEN NULL ELSE actor_username END, subject_username = CASE WHEN subject_account_id = account THEN NULL ELSE subject_username END, metadata = CASE WHEN subject_account_id = account THEN metadata - ARRAY['ip', 'from_expertise', 'to_expertise'] ELSE metadata - 'ip' END WHERE (actor_account_id = account OR subject_account_id = account) AND (actor_username IS NOT NULL AND actor_account_id = account OR subject_username IS NOT NULL AND subject_account_id = account OR metadata ? 'ip' OR subject_account_id = account AND metadata ?| ARRAY['from_expertise', 'to_expertise']);
    GET DIAGNOSTICS changed = ROW_COUNT;
    INSERT INTO audit_events (event_type, actor_account_id, actor_service, metadata, resource_type, resource_id)
    VALUES ('audit_account_anonymized', admin, CASE WHEN admin IS NULL THEN 'retention' END,
            jsonb_build_object('events', changed), 'account', account::text);
    RETURN changed;
END $$;

-- Called by retention_purge_ticket. Refuses a ticket whose content is still
-- stored; returns the number of events changed.
CREATE FUNCTION audit_redact_ticket_text(ticket uuid) RETURNS bigint LANGUAGE plpgsql AS $$
DECLARE changed bigint;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM tickets WHERE id = ticket AND content_purged_at IS NOT NULL) THEN
        RAISE EXCEPTION 'the content of ticket % has not been removed', ticket
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    UPDATE audit_events SET metadata = jsonb_set(metadata, '{reason}', to_jsonb(retention_marker())) WHERE ticket_id = ticket AND event_type = 'ticket_cancel' AND metadata ? 'reason' AND metadata->>'reason' <> retention_marker();
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed > 0 THEN
        INSERT INTO audit_events (event_type, actor_service, metadata)
        VALUES ('audit_ticket_text_redacted', 'retention',
                jsonb_build_object('ticket_id', ticket, 'events', changed));
    END IF;
    RETURN changed;
END $$;

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
