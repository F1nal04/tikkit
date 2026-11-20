-- Operational metadata for jobs triggered by ticket events. Existing events
-- retain NULL; workers assign a fresh correlation ID when processing them.
ALTER TABLE ticket_events ADD COLUMN request_id uuid;
