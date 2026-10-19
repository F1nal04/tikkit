-- Operator queues (apps/api/src/queues.rs): open work in creation order, so a
-- page of any queue, filtered or not, walks this index instead of scanning every
-- ticket. Visibility and view filters are applied on top (docs/queues.md).
CREATE INDEX tickets_open_work_idx ON tickets (created_at, id)
    WHERE status IN ('queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation');
