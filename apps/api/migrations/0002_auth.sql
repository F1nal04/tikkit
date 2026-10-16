-- Setup state, sessions, and the security audit log. See docs/auth.md.

-- Singleton organization row; setup_completed_at closes first-admin bootstrap for good.
CREATE TABLE organization (
    id                 boolean PRIMARY KEY DEFAULT true CHECK (id),
    setup_completed_at timestamptz
);
INSERT INTO organization DEFAULT VALUES;

-- Only the SHA-256 of the cookie token is stored. Rotation marks a row rotated_at
-- (valid for a short grace period) and inserts its successor (rotated_from).
CREATE TABLE sessions (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id   uuid NOT NULL REFERENCES accounts (id),
    token_hash   bytea NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    revoked_at   timestamptz,
    rotated_at   timestamptz,
    rotated_from uuid REFERENCES sessions (id),
    user_agent   text,
    ip           text
);
CREATE INDEX sessions_account_idx ON sessions (account_id);
CREATE INDEX sessions_expires_idx ON sessions (expires_at);
CREATE INDEX sessions_rotated_from_idx ON sessions (rotated_from) WHERE rotated_from IS NOT NULL;

CREATE TABLE audit_events (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_account_id   uuid REFERENCES accounts (id), -- NULL for anonymous/system
    event_type         text NOT NULL,
    subject_account_id uuid REFERENCES accounts (id),
    metadata           jsonb NOT NULL DEFAULT '{}', -- never secrets, passwords, or tokens
    created_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_events_created_idx ON audit_events (created_at, id);
CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION reject_mutation();
CREATE TRIGGER audit_events_no_truncate
    BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();
