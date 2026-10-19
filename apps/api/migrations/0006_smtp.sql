-- Admin-configured outbound SMTP. One connection per deployment.
-- password_ciphertext is AES-256-GCM (12-byte nonce || ciphertext and tag), never plaintext.
-- See docs/schema.md.

CREATE TABLE smtp_connection (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    host text NOT NULL CHECK (
        char_length(host) BETWEEN 1 AND 253
        AND host !~ '[[:space:][:cntrl:]@/]'
    ),
    port integer NOT NULL CHECK (port BETWEEN 1 AND 65535),
    security text NOT NULL CHECK (security IN ('starttls', 'implicit_tls')),
    sender_name text NOT NULL CHECK (
        char_length(btrim(sender_name)) BETWEEN 1 AND 128
        AND sender_name !~ '[[:cntrl:]]'
    ),
    sender_address text NOT NULL CHECK (char_length(sender_address) BETWEEN 3 AND 254),
    username text NOT NULL CHECK (
        char_length(username) BETWEEN 1 AND 256
        AND username !~ '[[:cntrl:]]'
    ),
    password_ciphertext bytea NOT NULL CHECK (octet_length(password_ciphertext) >= 28),
    enabled boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by uuid REFERENCES accounts (id),
    -- Stable error codes only (`smtp_auth_failed`, ...). Never a password, token, or address.
    last_delivery_error text CHECK (last_delivery_error IS NULL OR last_delivery_error !~ '[[:cntrl:]]'),
    last_delivery_error_at timestamptz,
    last_test_at timestamptz,
    last_test_error text CHECK (last_test_error IS NULL OR last_test_error !~ '[[:cntrl:]]')
);

-- Successful sends. A repeat of the same key does not send again. Callers (the
-- notification outbox) choose the key; this table stores no message content.
CREATE TABLE smtp_deliveries (
    idempotency_key text PRIMARY KEY CHECK (
        char_length(idempotency_key) BETWEEN 1 AND 200
        AND idempotency_key !~ '[[:cntrl:]]'
    ),
    sent_at timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER smtp_deliveries_append_only
    BEFORE UPDATE OR DELETE ON smtp_deliveries
    FOR EACH ROW EXECUTE FUNCTION reject_mutation();
