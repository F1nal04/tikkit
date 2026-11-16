-- AI usage safeguards: Admin-set budgets and limits, one usage record per guarded
-- provider call, and the provider circuit breaker. All three are shared state for
-- the API process and its workers, so budgets, the concurrency limit, and the
-- breaker hold across processes. See docs/schema.md and docs/ai-usage.md.

CREATE TABLE ai_limits (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    -- Budgets count over a rolling window of this many hours.
    window_hours integer NOT NULL DEFAULT 24 CHECK (window_hours BETWEEN 1 AND 744),
    -- NULL means no limit. 0 admits nothing.
    org_request_limit integer DEFAULT 500 CHECK (org_request_limit BETWEEN 0 AND 1000000),
    user_request_limit integer DEFAULT 10 CHECK (user_request_limit BETWEEN 0 AND 1000000),
    org_token_limit bigint CHECK (org_token_limit BETWEEN 0 AND 1000000000000),
    user_token_limit bigint CHECK (user_token_limit BETWEEN 0 AND 1000000000000),
    -- Provider calls in flight at once, over every process.
    max_concurrency integer NOT NULL DEFAULT 2 CHECK (max_concurrency BETWEEN 1 AND 64),
    -- How long a call may wait for a free slot before it is refused.
    concurrency_wait_seconds integer NOT NULL DEFAULT 30
        CHECK (concurrency_wait_seconds BETWEEN 0 AND 300),
    -- Consecutive provider failures that open the circuit, and how long it stays open.
    breaker_failure_threshold integer NOT NULL DEFAULT 5
        CHECK (breaker_failure_threshold BETWEEN 1 AND 100),
    breaker_cooldown_seconds integer NOT NULL DEFAULT 60
        CHECK (breaker_cooldown_seconds BETWEEN 1 AND 86400),
    -- Usage records older than this are deleted. Never shorter than the window.
    retention_days integer NOT NULL DEFAULT 90 CHECK (retention_days BETWEEN 1 AND 3650),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by uuid REFERENCES accounts (id),
    CHECK (retention_days * 24 >= window_hours)
);
INSERT INTO ai_limits DEFAULT VALUES;

-- One row per guarded provider call or refusal. Metadata only: never prompt text,
-- ticket content, image bytes, or provider message text.
CREATE TABLE ai_usage (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The requester the call was made for.
    account_id uuid NOT NULL REFERENCES accounts (id),
    provider text NOT NULL CHECK (provider IN ('openai', 'gemini', 'anthropic')),
    model text NOT NULL CHECK (model ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,99}$'),
    image_count integer NOT NULL CHECK (image_count >= 0),
    -- reserved: admitted and in flight. succeeded / failed: the call ended.
    -- refused: a guard stopped it before any provider call.
    status text NOT NULL CHECK (status IN ('reserved', 'succeeded', 'failed', 'refused')),
    -- Stable code of the failure or refusal (docs/ai-usage.md). NULL otherwise.
    outcome text CHECK (outcome ~ '^[a-z_]{1,64}$'),
    -- HTTP status and short error identifier of the provider response, if any.
    http_status integer CHECK (http_status BETWEEN 100 AND 599),
    provider_code text CHECK (provider_code ~ '^[A-Za-z0-9_.-]{1,64}$'),
    -- Exactly what the provider reported. NULL: it did not report that value.
    input_tokens bigint CHECK (input_tokens >= 0),
    output_tokens bigint CHECK (output_tokens >= 0),
    total_tokens bigint CHECK (total_tokens >= 0),
    -- What this row counts against token budgets, and where the number comes from:
    -- estimated (Tikkit's own admission estimate), reported (provider figures),
    -- none (nothing was generated). Never a cost.
    budget_tokens bigint NOT NULL CHECK (budget_tokens >= 0),
    budget_basis text NOT NULL CHECK (budget_basis IN ('estimated', 'reported', 'none')),
    -- Whether this call was the half-open probe of the circuit breaker.
    breaker_probe boolean NOT NULL DEFAULT false,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    -- A reserved row holds its concurrency slot until this time at the latest,
    -- so a crashed process cannot keep a slot forever.
    lease_expires_at timestamptz,
    CHECK ((status = 'reserved') = (finished_at IS NULL)),
    CHECK ((status = 'reserved') = (lease_expires_at IS NOT NULL)),
    CHECK ((status IN ('failed', 'refused')) = (outcome IS NOT NULL))
);
CREATE INDEX ai_usage_started_idx ON ai_usage (started_at);
CREATE INDEX ai_usage_account_idx ON ai_usage (account_id, started_at);
CREATE INDEX ai_usage_reserved_idx ON ai_usage (lease_expires_at) WHERE status = 'reserved';

-- Circuit breaker state per provider. No row means closed with no failures.
CREATE TABLE ai_circuit (
    provider text PRIMARY KEY CHECK (provider IN ('openai', 'gemini', 'anthropic')),
    consecutive_failures integer NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    -- Set while the circuit is open. Once it has passed, one probe call is let through.
    open_until timestamptz,
    opened_at timestamptz,
    -- The probe in flight. Another probe is allowed once its lease has passed.
    probe_expires_at timestamptz,
    CHECK ((open_until IS NULL) = (opened_at IS NULL)),
    CHECK (probe_expires_at IS NULL OR open_until IS NOT NULL)
);
