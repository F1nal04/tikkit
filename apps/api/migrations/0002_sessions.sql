-- Server-side login sessions (issue #9). See docs/api.md "Authentication
-- and sessions" for the policy (idle/absolute expiry, rotation, revocation).
--
-- The browser holds a random 32-byte token; only its SHA-256 digest is
-- stored, so a database leak does not yield usable session cookies.
-- Rotation moves the current digest to previous_token_hash, which stays
-- accepted until previous_token_valid_until (a short grace period for
-- requests that were already in flight with the old cookie).

CREATE TABLE sessions (
    id                         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id                 uuid        NOT NULL REFERENCES accounts (id),
    token_hash                 bytea       NOT NULL CHECK (octet_length(token_hash) = 32),
    previous_token_hash        bytea       CHECK (previous_token_hash IS NULL OR octet_length(previous_token_hash) = 32),
    previous_token_valid_until timestamptz,
    created_at                 timestamptz NOT NULL DEFAULT now(),
    -- Updated at most about once a minute; drives the idle timeout.
    last_seen_at               timestamptz NOT NULL DEFAULT now(),
    rotated_at                 timestamptz NOT NULL DEFAULT now(),
    -- Absolute expiry, fixed at creation.
    expires_at                 timestamptz NOT NULL,
    revoked_at                 timestamptz,
    -- Stable snake_case reason, e.g. logout, login_replaced, account_inactive,
    -- account_deactivated, password_changed.
    revoke_reason              text        CHECK (revoke_reason IS NULL OR revoke_reason ~ '^[a-z][a-z0-9_]{0,63}$'),
    CONSTRAINT sessions_token_hash_key UNIQUE (token_hash),
    CONSTRAINT sessions_previous_token_complete
        CHECK ((previous_token_hash IS NULL) = (previous_token_valid_until IS NULL)),
    CONSTRAINT sessions_revocation_complete
        CHECK ((revoked_at IS NULL) = (revoke_reason IS NULL)),
    CONSTRAINT sessions_lifetime CHECK (expires_at > created_at)
);

CREATE UNIQUE INDEX sessions_previous_token_hash_key
    ON sessions (previous_token_hash) WHERE previous_token_hash IS NOT NULL;
CREATE INDEX sessions_unrevoked_by_account
    ON sessions (account_id) WHERE revoked_at IS NULL;
CREATE INDEX sessions_expires_idx ON sessions (expires_at);

-- Revocation is final: a revoked session can never be revived or re-keyed,
-- and session ownership never changes.
CREATE FUNCTION tikkit_guard_session_update() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.account_id IS DISTINCT FROM OLD.account_id THEN
        RAISE EXCEPTION 'session ownership is immutable'
            USING ERRCODE = 'restrict_violation', CONSTRAINT = 'sessions_owner_immutable';
    END IF;
    IF OLD.revoked_at IS NOT NULL THEN
        RAISE EXCEPTION 'revoked sessions cannot be modified'
            USING ERRCODE = 'restrict_violation', CONSTRAINT = 'sessions_revocation_final';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER sessions_guard
    BEFORE UPDATE ON sessions
    FOR EACH ROW EXECUTE FUNCTION tikkit_guard_session_update();
