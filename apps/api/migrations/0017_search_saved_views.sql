-- Staff ticket search and saved views (docs/queues.md "Search", "Saved views").
-- pg_trgm is a trusted extension: the database owner may create it without superuser.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Substring search (`ILIKE '%…%'`) over the frozen intake text. The expression must
-- match `queues::search_sql` exactly for the planner to use the index.
CREATE INDEX tickets_search_trgm_idx ON tickets USING gin (
    (subject || ' ' || description || ' ' || coalesce(intake_snapshot #>> '{catalog,category_name}', ''))
    gin_trgm_ops);
CREATE INDEX accounts_search_trgm_idx ON accounts USING gin ((username || ' ' || display_name) gin_trgm_ops);
CREATE INDEX ticket_messages_body_trgm_idx ON ticket_messages USING gin (body gin_trgm_ops);

-- A staff account's named queue filters. Only the owner reads or deletes them;
-- references are re-checked against the current catalog and teams when listed.
CREATE TABLE saved_views (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_account_id uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name             text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80 AND btrim(name) = name),
    filters          jsonb NOT NULL CHECK (jsonb_typeof(filters) = 'object'),
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_account_id, name)
);
