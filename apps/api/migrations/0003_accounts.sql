-- Invitations and password recovery. See docs/auth.md.
-- Tokens are stored as SHA-256 only, like session tokens.

CREATE TABLE invitations (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash  bytea NOT NULL UNIQUE,
    role        account_role NOT NULL DEFAULT 'user', -- applied on acceptance; never client-supplied
    email       text,                                 -- where the link was sent, if anywhere
    language    text NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'de')),
    created_by  uuid NOT NULL REFERENCES accounts (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    revoked_at  timestamptz,
    accepted_at timestamptz,
    account_id  uuid UNIQUE REFERENCES accounts (id), -- the account created from it
    CHECK ((accepted_at IS NULL) = (account_id IS NULL)),
    CHECK (accepted_at IS NULL OR revoked_at IS NULL)
);
CREATE INDEX invitations_pending_idx ON invitations (created_at, id)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;

-- Bound to one account; every unused token of that account dies on a successful reset.
CREATE TABLE password_resets (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id uuid NOT NULL REFERENCES accounts (id),
    token_hash bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at    timestamptz
);
CREATE INDEX password_resets_account_idx ON password_resets (account_id) WHERE used_at IS NULL;
