-- A single plain-text solution for read-only AI history. Provider processing is
-- separate work; no requester endpoint can write this value.
ALTER TABLE tickets ADD COLUMN ai_solution text
    CHECK (ai_solution IS NULL OR (length(btrim(ai_solution)) > 0 AND length(ai_solution) <= 100000));

CREATE FUNCTION freeze_ai_solution() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.ai_solution IS DISTINCT FROM OLD.ai_solution
       AND (OLD.ai_solution IS NOT NULL OR OLD.status NOT IN ('ai_processing', 'awaiting_ai_decision')) THEN
        RAISE EXCEPTION 'AI solution is immutable' USING ERRCODE = '23001';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER tickets_ai_solution_immutable BEFORE UPDATE ON tickets
    FOR EACH ROW EXECUTE FUNCTION freeze_ai_solution();
