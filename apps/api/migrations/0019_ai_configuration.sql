-- Optional Admin-managed BYOK AI configuration. One row of settings per deployment
-- and at most one credential per supported provider.
-- key_ciphertext is AES-256-GCM (12-byte nonce || ciphertext and tag) under the
-- external ENCRYPTION_KEY, with the provider name as associated data: a ciphertext
-- copied to another provider's row does not decrypt. Never plaintext.
-- See docs/schema.md and docs/ai-configuration.md.

CREATE TABLE ai_settings (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    -- AI is off until an Admin enables it.
    enabled boolean NOT NULL DEFAULT false,
    provider text CHECK (provider IN ('openai', 'gemini', 'anthropic')),
    model text CHECK (model ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,99}$'),
    -- Result of the last Admin connection test of the selected provider and model.
    -- Stable error codes only (`ai_provider_auth_failed`, ...). Never a key or provider text.
    connection_status text NOT NULL DEFAULT 'untested'
        CHECK (connection_status IN ('untested', 'ok', 'failed')),
    connection_error text CHECK (connection_error ~ '^[a-z_]{1,64}$'),
    connection_tested_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by uuid REFERENCES accounts (id),
    CHECK (NOT enabled OR (provider IS NOT NULL AND model IS NOT NULL)),
    CHECK ((connection_status = 'failed') = (connection_error IS NOT NULL)),
    CHECK ((connection_status = 'untested') = (connection_tested_at IS NULL))
);
INSERT INTO ai_settings DEFAULT VALUES;

CREATE TABLE ai_credentials (
    provider text PRIMARY KEY CHECK (provider IN ('openai', 'gemini', 'anthropic')),
    key_ciphertext bytea NOT NULL CHECK (octet_length(key_ciphertext) >= 29),
    -- Last four characters of a long key, or empty. Display only.
    key_hint text NOT NULL CHECK (char_length(key_hint) <= 4 AND key_hint !~ '[[:space:][:cntrl:]]'),
    set_at timestamptz NOT NULL DEFAULT now(),
    set_by uuid REFERENCES accounts (id)
);
