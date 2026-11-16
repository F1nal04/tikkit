-- Requester decisions on the AI suggestion (#37). See docs/ai-decisions.md.
--
-- The decision itself is the immutable `ai_solved` / `ai_not_solved` ticket
-- event written by `lifecycle::transition`; its payload names the attempt
-- (`attempt_id`) and its `created_at` is the confirmation time. Attempt rows
-- stay untouched: history grows by appending events only.

-- A ticket has one suggestion and therefore at most one decision on it, whatever
-- races or retries: a second `ai_solved` or `ai_not_solved` event cannot exist.
CREATE UNIQUE INDEX ticket_events_one_ai_decision
    ON ticket_events (ticket_id) WHERE event_type IN ('ai_solved', 'ai_not_solved');

-- Outcome statistics and the Admin record list read AI events by time.
CREATE INDEX ticket_events_ai_outcome_idx ON ticket_events (created_at)
    WHERE event_type IN ('ai_solved', 'ai_not_solved', 'skip_ai', 'ai_failed');

-- An AI-solved record is final in every column, not only status, team, and
-- assignee: no priority change, no `updated_at` bump, and the confirmation
-- (`resolution_source`, `closure_reason`) cannot be erased. Replaces the
-- function of migration 0008 with that one additional rule.
CREATE OR REPLACE FUNCTION check_ticket_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
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

-- Nothing is appended to an AI-solved record either. The one `ai_solved` event
-- is inserted after the status change; the index above limits it to one.
CREATE FUNCTION check_ticket_event_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.event_type <> 'ai_solved'
        AND EXISTS (SELECT 1 FROM tickets WHERE id = NEW.ticket_id AND status = 'ai_solved') THEN
        RAISE EXCEPTION 'ai_solved tickets are final' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER ticket_events_ai_solved_final BEFORE INSERT ON ticket_events
    FOR EACH ROW EXECUTE FUNCTION check_ticket_event_insert();
