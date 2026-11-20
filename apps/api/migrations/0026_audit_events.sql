-- Append-only audit events for privileged and ticket actions (#42). See docs/audit.md.
--
-- `audit_events` (migration 0002) stays the one audit log. This migration adds
-- the context every event needs (request ID, actor snapshot or service name,
-- resource), mirrors every `ticket_events` row into it in the writer's
-- transaction, and replaces the append-only guard with one that leaves two
-- explicit paths for the retention feature (`audit_retention_purge`,
-- `audit_anonymize_account`).

ALTER TABLE audit_events
    -- The correlation ID of the request that wrote the event, or that caused the
    -- job that wrote it (same meaning and type as `ticket_events.request_id`, 0025).
    ADD COLUMN request_id       uuid,
    -- The acting account as it was when the event was written (filled by trigger).
    ADD COLUMN actor_username   text,
    ADD COLUMN actor_role       account_role,
    ADD COLUMN actor_team_id    uuid, -- no foreign key: history outlives the row it names
    -- Named identity when no account acts: `anonymous` or a background service.
    ADD COLUMN actor_service    text CHECK (char_length(actor_service) BETWEEN 1 AND 64),
    ADD COLUMN subject_username text,
    -- What the event is about: a stable type and that resource's identifier.
    ADD COLUMN resource_type    text CHECK (char_length(resource_type) BETWEEN 1 AND 64),
    ADD COLUMN resource_id      text CHECK (char_length(resource_id) BETWEEN 1 AND 128),
    ADD COLUMN ticket_id        uuid; -- no foreign key, same reason

-- The resource of an event, from the conventions the writers already follow.
CREATE FUNCTION audit_resource_of(
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
    ELSIF subject IS NOT NULL THEN
        resource_type := 'account'; resource_id := subject::text;
    END IF;
    resource_id := left(resource_id, 128);
END $$;

-- The background service behind a ticket event without an acting account.
CREATE FUNCTION audit_ticket_service(event_type text) RETURNS text LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE
        WHEN event_type = 'confirmation_timeout' THEN 'auto_close'
        WHEN event_type IN ('ai_suggestion_ready', 'ai_failed') THEN 'ai_attempts'
        ELSE 'system' END
$$;

-- The audit metadata of a ticket event: identifiers, statuses, and policy
-- values only. Keys are allow-listed so text a future payload may carry (like
-- today's proposal `summary`) never reaches the audit log by default. The
-- requester's cancellation `reason` is kept on purpose (issue #42).
CREATE FUNCTION audit_ticket_metadata(e ticket_events) RETURNS jsonb LANGUAGE sql STABLE AS $$
    SELECT COALESCE((
               SELECT jsonb_object_agg(p.key, p.value) FROM jsonb_each(e.payload) p
               WHERE p.key IN (
                   'route', 'attempt_id', 'resolution_source', 'closure_reason',
                   'proposal_version', 'timeout_hours', 'deadline', 'reason', 'action',
                   'previous_team_id', 'team_id', 'previous_assignee_account_id',
                   'assignee_account_id', 'previous_priority_id', 'previous_priority_name',
                   'priority_id', 'priority_name', 'message_id')
           ), '{}')
        || jsonb_strip_nulls(jsonb_build_object(
               'ticket_event_id', e.id, 'from_status', e.from_status, 'to_status', e.to_status))
        || CASE e.event_type
               -- The requester's original selection, whatever staff change later.
               WHEN 'submit' THEN COALESCE((
                   SELECT jsonb_build_object(
                       'category_id', t.category_id,
                       'priority_id', COALESCE(t.intake_snapshot#>>'{catalog,priority_id}', t.priority_id::text),
                       'priority_name', COALESCE(t.intake_snapshot#>>'{catalog,priority_name}',
                                                 (SELECT name FROM priorities WHERE id = t.priority_id)))
                   FROM tickets t WHERE t.id = e.ticket_id), '{}')
               -- The proposal that timed out: its snapshotted deadline and timeout.
               WHEN 'confirmation_timeout' THEN COALESCE((
                   SELECT jsonb_build_object('deadline', p.payload->'deadline',
                                             'timeout_hours', p.payload->'timeout_hours')
                   FROM ticket_events p
                   WHERE p.ticket_id = e.ticket_id AND p.event_type = 'propose_resolution' AND p.id < e.id
                   ORDER BY p.id DESC LIMIT 1), '{}')
               -- Why AI handed off: the attempt's stable outcome code (never a
               -- provider message). The attempt is ended before the event is written.
               WHEN 'ai_failed' THEN COALESCE((
                   SELECT jsonb_build_object('outcome', a.outcome)
                   FROM ai_attempts a WHERE a.ticket_id = e.ticket_id AND a.outcome IS NOT NULL), '{}')
               ELSE '{}'::jsonb END
$$;

-- History written before this migration: usernames as they are now (roles and
-- teams at the time are unknown and stay NULL), a service name, the resource.
DROP TRIGGER audit_events_append_only ON audit_events;
UPDATE audit_events e SET
    actor_username = (SELECT username FROM accounts WHERE id = e.actor_account_id),
    subject_username = (SELECT username FROM accounts WHERE id = e.subject_account_id),
    actor_service = CASE
        WHEN e.actor_account_id IS NOT NULL THEN NULL
        WHEN e.event_type IN ('login_failed', 'recovery_requested') THEN 'anonymous'
        WHEN e.event_type = 'ai_usage_purged' THEN 'ai_usage_retention'
        WHEN e.event_type = 'screenshots_expired' THEN 'screenshot_cleanup'
        WHEN e.event_type = 'ai_attempt_orphaned' THEN 'ai_attempts'
        ELSE 'system' END,
    resource_type = r.resource_type,
    resource_id = r.resource_id,
    ticket_id = CASE WHEN r.resource_type = 'ticket'
                      AND r.resource_id ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                     THEN r.resource_id::uuid END
FROM (SELECT x.id, (audit_resource_of(x.event_type, x.subject_account_id, x.metadata)).*
      FROM audit_events x) r
WHERE r.id = e.id;

INSERT INTO audit_events (event_type, actor_account_id, actor_username, actor_service, metadata,
                          created_at, request_id, resource_type, resource_id, ticket_id)
SELECT 'ticket_' || e.event_type, e.actor_account_id, a.username,
       CASE WHEN e.actor_account_id IS NULL THEN audit_ticket_service(e.event_type) END,
       audit_ticket_metadata(e), e.created_at, e.request_id, 'ticket', e.ticket_id::text, e.ticket_id
FROM ticket_events e LEFT JOIN accounts a ON a.id = e.actor_account_id
ORDER BY e.id;

-- Every event names who acted: an account, or a service.
ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_actor_identity CHECK ((actor_account_id IS NULL) <> (actor_service IS NULL));

-- Fills the actor snapshot and the resource, so no writer can forget them.
CREATE FUNCTION audit_events_fill() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r record;
BEGIN
    IF NEW.actor_account_id IS NOT NULL THEN
        NEW.actor_service := NULL;
        SELECT a.username, a.role, m.team_id
          INTO NEW.actor_username, NEW.actor_role, NEW.actor_team_id
          FROM accounts a LEFT JOIN team_memberships m ON m.account_id = a.id
         WHERE a.id = NEW.actor_account_id;
    ELSE
        NEW.actor_service := COALESCE(NEW.actor_service, 'system');
        NEW.actor_username := NULL; NEW.actor_role := NULL; NEW.actor_team_id := NULL;
    END IF;
    NEW.subject_username := (SELECT username FROM accounts WHERE id = NEW.subject_account_id);
    IF NEW.resource_type IS NULL THEN
        r := audit_resource_of(NEW.event_type, NEW.subject_account_id, NEW.metadata);
        NEW.resource_type := r.resource_type;
        NEW.resource_id := r.resource_id;
    END IF;
    IF NEW.ticket_id IS NULL AND NEW.resource_type = 'ticket'
        AND NEW.resource_id ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
        NEW.ticket_id := NEW.resource_id::uuid;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER audit_events_fill BEFORE INSERT ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_fill();

-- Every ticket event is also an audit event, in the same transaction: a ticket
-- change cannot commit without it, and a rollback removes both.
CREATE FUNCTION audit_ticket_event() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE meta jsonb := audit_ticket_metadata(NEW);
BEGIN
    -- The policy that was in effect, for the two policy-gated transitions.
    IF NEW.event_type = 'reopen' THEN
        meta := meta || (SELECT jsonb_build_object('allow_reopening', allow_reopening) FROM organization);
    ELSIF NEW.event_type = 'cancel' THEN
        meta := meta || (SELECT jsonb_build_object('cancellation_policy', requester_cancellation_policy)
                         FROM organization);
    END IF;
    INSERT INTO audit_events (event_type, actor_account_id, actor_service, metadata, created_at,
                              request_id, resource_type, resource_id, ticket_id)
    VALUES ('ticket_' || NEW.event_type, NEW.actor_account_id,
            CASE WHEN NEW.actor_account_id IS NULL THEN audit_ticket_service(NEW.event_type) END,
            meta, NEW.created_at, NEW.request_id, 'ticket', NEW.ticket_id::text, NEW.ticket_id);
    RETURN NULL;
END $$;
CREATE TRIGGER ticket_events_audit AFTER INSERT ON ticket_events
    FOR EACH ROW EXECUTE FUNCTION audit_ticket_event();

-- Append-only. UPDATE, DELETE, and TRUNCATE (trigger of 0002) are refused, with
-- two exceptions that are functions of this migration, not settings:
--
--   * `audit_retention_purge` may delete events older than the retention floor;
--   * `audit_anonymize_account` may blank one account's username snapshots and
--     the `ip` key of its events.
--
-- The guard recognises them by the call stack (PG_CONTEXT): the row change
-- must come from the one statement inside that function. A session cannot put
-- that frame on the stack by setting a variable or wrapping its own statement
-- in a block or function; it would have to replace the function or the
-- trigger, which is DDL. On top of that the guard checks every row itself, so
-- even from inside those functions nothing else can change.
--
-- Limit: the application's database role owns these objects. Whoever can run
-- DDL as that role (or a superuser) can drop or replace the trigger. Separate
-- owner and runtime roles would close that; it is not part of this migration.
CREATE FUNCTION audit_retention_floor() RETURNS interval LANGUAGE sql IMMUTABLE AS $$
    SELECT interval '30 days'
$$;

CREATE FUNCTION audit_events_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stack text;
BEGIN
    GET DIAGNOSTICS stack = PG_CONTEXT;
    IF TG_OP = 'DELETE'
        AND stack ~ ('^PL/pgSQL function audit_events_guard\(\) line \d+ at GET DIAGNOSTICS\n'
                     || 'SQL statement "DELETE FROM audit_events [^\n]*"\n'
                     || 'PL/pgSQL function audit_retention_purge\(timestamp with time zone,uuid\) line \d+ at SQL statement(\n|$)')
        AND OLD.event_type <> 'audit_retention_purged'
        AND OLD.created_at < now() - audit_retention_floor() THEN
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE'
        AND stack ~ ('^PL/pgSQL function audit_events_guard\(\) line \d+ at GET DIAGNOSTICS\n'
                     || 'SQL statement "UPDATE audit_events [^\n]*"\n'
                     || 'PL/pgSQL function audit_anonymize_account\(uuid,uuid\) line \d+ at SQL statement(\n|$)')
        -- Only the username snapshots may become NULL and only `ip` may go.
        AND (NEW.actor_username IS NULL OR NEW.actor_username IS NOT DISTINCT FROM OLD.actor_username)
        AND (NEW.subject_username IS NULL OR NEW.subject_username IS NOT DISTINCT FROM OLD.subject_username)
        AND NEW.metadata IN (OLD.metadata, OLD.metadata - 'ip')
        AND (NEW.id, NEW.event_type, NEW.created_at) = (OLD.id, OLD.event_type, OLD.created_at)
        AND (NEW.actor_account_id, NEW.actor_role, NEW.actor_team_id, NEW.actor_service,
             NEW.subject_account_id, NEW.request_id, NEW.resource_type, NEW.resource_id, NEW.ticket_id)
            IS NOT DISTINCT FROM
            (OLD.actor_account_id, OLD.actor_role, OLD.actor_team_id, OLD.actor_service,
             OLD.subject_account_id, OLD.request_id, OLD.resource_type, OLD.resource_id, OLD.ticket_id) THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only' USING ERRCODE = 'restrict_violation';
END $$;
CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_guard();

-- Who may run retention and anonymization: an active Admin account, or NULL
-- for the named retention service.
CREATE FUNCTION audit_require_admin(admin uuid) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF admin IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM accounts WHERE id = admin AND role = 'admin' AND is_active) THEN
        RAISE EXCEPTION 'only an active Admin may change audit history'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
END $$;

-- Retention (#43): deletes events older than `cutoff`, which must itself be at
-- least `audit_retention_floor()` (30 days) in the past, and appends who did
-- it, the cutoff, and the count in the same call. `audit_retention_purged`
-- events are never deleted. `admin` NULL means the retention job.
CREATE FUNCTION audit_retention_purge(cutoff timestamptz, admin uuid) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE removed bigint;
BEGIN
    PERFORM audit_require_admin(admin);
    IF cutoff IS NULL OR cutoff > now() - audit_retention_floor() THEN
        RAISE EXCEPTION 'audit retention cutoff must be at least % in the past', audit_retention_floor()
            USING ERRCODE = 'check_violation';
    END IF;
    DELETE FROM audit_events WHERE created_at < cutoff AND event_type <> 'audit_retention_purged';
    GET DIAGNOSTICS removed = ROW_COUNT;
    INSERT INTO audit_events (event_type, actor_account_id, actor_service, metadata)
    VALUES ('audit_retention_purged', admin, CASE WHEN admin IS NULL THEN 'retention' END,
            jsonb_build_object('cutoff', cutoff, 'deleted', removed));
    RETURN removed;
END $$;

-- Anonymization (#43): blanks the username snapshots of `account` and removes
-- the `ip` key from the events it acted in or is the subject of. Account IDs,
-- actions, timestamps, and everything else stay. Appends
-- `audit_account_anonymized` (about the account, without a username snapshot)
-- and returns the number of events changed.
CREATE FUNCTION audit_anonymize_account(account uuid, admin uuid) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE changed bigint;
BEGIN
    PERFORM audit_require_admin(admin);
    IF account IS NULL OR NOT EXISTS (SELECT 1 FROM accounts WHERE id = account) THEN
        RAISE EXCEPTION 'unknown account' USING ERRCODE = 'check_violation';
    END IF;
    UPDATE audit_events SET actor_username = CASE WHEN actor_account_id = account THEN NULL ELSE actor_username END, subject_username = CASE WHEN subject_account_id = account THEN NULL ELSE subject_username END, metadata = metadata - 'ip' WHERE (actor_account_id = account OR subject_account_id = account) AND (actor_username IS NOT NULL AND actor_account_id = account OR subject_username IS NOT NULL AND subject_account_id = account OR metadata ? 'ip');
    GET DIAGNOSTICS changed = ROW_COUNT;
    INSERT INTO audit_events (event_type, actor_account_id, actor_service, metadata, resource_type, resource_id)
    VALUES ('audit_account_anonymized', admin, CASE WHEN admin IS NULL THEN 'retention' END,
            jsonb_build_object('events', changed), 'account', account::text);
    RETURN changed;
END $$;

CREATE INDEX audit_events_actor_idx ON audit_events (actor_account_id, created_at, id)
    WHERE actor_account_id IS NOT NULL;
CREATE INDEX audit_events_type_idx ON audit_events (event_type, created_at, id);
CREATE INDEX audit_events_ticket_idx ON audit_events (ticket_id, created_at, id)
    WHERE ticket_id IS NOT NULL;
CREATE INDEX audit_events_resource_idx ON audit_events (resource_type, resource_id, created_at, id);
CREATE INDEX audit_events_request_idx ON audit_events (request_id) WHERE request_id IS NOT NULL;
