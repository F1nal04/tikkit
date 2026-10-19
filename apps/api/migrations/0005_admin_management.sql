-- Runtime organization policy; no credentials or provider configuration here.
ALTER TABLE organization
    ADD COLUMN name text NOT NULL DEFAULT 'Tikkit' CHECK (length(btrim(name)) BETWEEN 1 AND 120),
    ADD COLUMN timezone text NOT NULL DEFAULT 'UTC' CHECK (length(timezone) BETWEEN 1 AND 100),
    ADD COLUMN default_language text NOT NULL DEFAULT 'en' CHECK (default_language IN ('en', 'de')),
    ADD COLUMN allow_reopening boolean NOT NULL DEFAULT false,
    ADD COLUMN requester_cancellation_policy text NOT NULL DEFAULT 'anytime_while_open'
        CHECK (requester_cancellation_policy IN ('anytime_while_open', 'before_work_starts', 'disabled')),
    ADD COLUMN resolution_confirmation_hours integer NOT NULL DEFAULT 168 CHECK (resolution_confirmation_hours > 0),
    ADD COLUMN allow_requester_ai_skip boolean NOT NULL DEFAULT false;

ALTER TABLE accounts ADD COLUMN expertise text CHECK (length(expertise) BETWEEN 1 AND 120);
CREATE INDEX invitations_history_idx ON invitations (created_at DESC, id DESC);
