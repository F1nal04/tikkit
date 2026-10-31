-- Ticket lifecycle: resolution provenance, versioned staff proposals, and
-- database backstops for the state machine in apps/api/src/lifecycle.rs.
-- Which transitions are allowed is decided in Rust only; see docs/lifecycle.md.

ALTER TABLE tickets
    -- Bumped by every staff resolution proposal; requester decisions and the
    -- confirmation timeout name the version they apply to.
    ADD COLUMN proposal_version      integer NOT NULL DEFAULT 0 CHECK (proposal_version >= 0),
    -- Snapshotted when a proposal is made; set exactly while awaiting confirmation.
    ADD COLUMN confirmation_deadline timestamptz,
    ADD COLUMN resolution_source     text CHECK (resolution_source IN ('ai', 'operator')),
    ADD COLUMN closure_reason        text CHECK (closure_reason IN ('requester_confirmed', 'confirmation_timeout')),
    -- Pre-triage and AI-solved tickets never have a team, so every handoff to
    -- humans starts in the shared general queue.
    ADD CONSTRAINT tickets_pre_triage_unrouted
        CHECK (status NOT IN ('draft', 'ai_processing', 'awaiting_ai_decision', 'ai_solved') OR team_id IS NULL),
    ADD CONSTRAINT tickets_deadline_while_awaiting
        CHECK ((status = 'awaiting_confirmation') = (confirmation_deadline IS NOT NULL)),
    ADD CONSTRAINT tickets_resolution_matches_status CHECK (CASE status
        WHEN 'ai_solved' THEN resolution_source IS NOT DISTINCT FROM 'ai'
                              AND closure_reason IS NOT DISTINCT FROM 'requester_confirmed'
        WHEN 'closed' THEN resolution_source IS NOT DISTINCT FROM 'operator' AND closure_reason IS NOT NULL
        ELSE resolution_source IS NULL AND closure_reason IS NULL END);

-- Due proposals for the auto-close worker.
CREATE INDEX tickets_confirmation_due_idx ON tickets (confirmation_deadline)
    WHERE status = 'awaiting_confirmation';

-- The requester never changes; submitted intake is frozen and never returns to
-- draft; AI-solved and cancelled records are final, except that an assignee who
-- leaves the team or is deactivated is cleared (team memberships are the
-- assignee's foreign key; the attributed assignment event keeps the history).
CREATE FUNCTION check_ticket_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.requester_account_id <> OLD.requester_account_id THEN
        RAISE EXCEPTION 'ticket requesters are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.status <> 'draft' AND (NEW.status = 'draft'
        OR (NEW.subject, NEW.description, NEW.category_id) IS DISTINCT FROM (OLD.subject, OLD.description, OLD.category_id)) THEN
        RAISE EXCEPTION 'submitted intake is frozen' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.status IN ('ai_solved', 'cancelled')
        AND (NEW.status, NEW.team_id, NEW.assignee_account_id) IS DISTINCT FROM (OLD.status, OLD.team_id, OLD.assignee_account_id)
        AND NOT ((NEW.status, NEW.team_id) IS NOT DISTINCT FROM (OLD.status, OLD.team_id)
                 AND NEW.assignee_account_id IS NULL) THEN
        RAISE EXCEPTION '% tickets are final', OLD.status USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tickets_lifecycle_guard
    BEFORE UPDATE ON tickets
    FOR EACH ROW EXECUTE FUNCTION check_ticket_update();
