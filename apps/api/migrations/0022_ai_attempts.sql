-- AI suggestion attempts: at most one logical attempt per submitted ticket, with
-- the immutable input it was (or would have been) sent with and the single
-- stored suggestion. Written by submission and by the attempt worker
-- (apps/api/src/ai_attempts.rs). See docs/ai-attempts.md and docs/schema.md.

CREATE TABLE ai_attempts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- One attempt per ticket, ever: no retry, regenerate, or second solution.
    ticket_id uuid NOT NULL UNIQUE REFERENCES tickets (id),
    -- processing: waiting for or running the provider call.
    -- suggestion_ready: the one suggestion is stored (explanation, steps).
    -- handed_off: no suggestion; the ticket went to the operator queue.
    -- cancelled: the requester cancelled the whole ticket while it was processing.
    state text NOT NULL DEFAULT 'processing'
        CHECK (state IN ('processing', 'suggestion_ready', 'handed_off', 'cancelled')),
    -- Stable code of why there is no suggestion (docs/ai-attempts.md).
    outcome text CHECK (outcome ~ '^[a-z_]{1,64}$'),
    -- The requester's language at submission; the suggestion is written in it.
    language text NOT NULL CHECK (language IN ('en', 'de')),
    -- The minimized input: category, subject, description, visible answers, and
    -- the intake screenshots (id, sha256, type, size). Never changes.
    input jsonb NOT NULL,
    -- The configuration the attempt was created under. A different selection or
    -- key invalidates it.
    provider text NOT NULL CHECK (provider IN ('openai', 'gemini', 'anthropic')),
    model text NOT NULL CHECK (model ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,99}$'),
    credential_set_at timestamptz NOT NULL,
    explanation text,
    steps jsonb,
    -- The guarded provider call that ended the attempt, while its usage row is retained.
    usage_id uuid REFERENCES ai_usage (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    -- Past this time the attempt is handed off, whatever a worker is doing.
    deadline_at timestamptz NOT NULL,
    -- Worker lease: another worker may take the attempt once it has passed.
    claimed_until timestamptz,
    claims integer NOT NULL DEFAULT 0 CHECK (claims >= 0),
    finished_at timestamptz,
    CHECK ((state = 'processing') = (finished_at IS NULL)),
    CHECK (state = 'processing' OR claimed_until IS NULL),
    CHECK ((state = 'suggestion_ready') = (explanation IS NOT NULL)),
    CHECK ((state = 'suggestion_ready') = (steps IS NOT NULL)),
    CHECK ((state IN ('handed_off', 'cancelled')) = (outcome IS NOT NULL)),
    CHECK (steps IS NULL OR jsonb_typeof(steps) = 'array')
);
CREATE INDEX ai_attempts_processing_idx ON ai_attempts (created_at) WHERE state = 'processing';
-- The sweep that hands off overdue or orphaned AI-stage tickets.
CREATE INDEX tickets_ai_processing_idx ON tickets (created_at) WHERE status = 'ai_processing';

-- Attempt history is immutable: the snapshot and its provenance never change, a
-- finished attempt never changes again (only its usage reference may be cleared
-- when the usage row is purged), and attempts are never deleted.
CREATE FUNCTION check_ai_attempt_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'AI attempts are never deleted' USING ERRCODE = 'restrict_violation';
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
CREATE TRIGGER ai_attempts_guard BEFORE UPDATE OR DELETE ON ai_attempts
    FOR EACH ROW EXECUTE FUNCTION check_ai_attempt_change();
