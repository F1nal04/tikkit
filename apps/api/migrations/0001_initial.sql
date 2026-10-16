-- Initial Tikkit schema. See docs/schema.md for identifiers and invariants.
-- Never edit an applied migration; add a new numbered file instead.

CREATE DOMAIN account_role AS text
    CHECK (VALUE IN ('user', 'operator', 'specialist', 'team_lead', 'admin'));

CREATE DOMAIN ticket_status AS text
    CHECK (VALUE IN ('draft', 'ai_processing', 'awaiting_ai_decision', 'queued', 'in_progress',
                     'waiting_for_requester', 'awaiting_confirmation', 'closed', 'cancelled', 'ai_solved'));

-- Shared guard for append-only tables.
CREATE FUNCTION reject_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = 'restrict_violation';
END $$;

CREATE TABLE accounts (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- No whitespace or control characters anywhere, judged after NFKC folding.
    username            text NOT NULL CHECK (username <> '' AND normalize(username, NFKC) !~ '[[:space:][:cntrl:]]'),
    username_normalized text GENERATED ALWAYS AS (lower(normalize(btrim(username), NFKC))) STORED UNIQUE,
    display_name        text NOT NULL,
    email               text NOT NULL, -- contact only, deliberately not unique
    password_hash       text NOT NULL,
    role                account_role NOT NULL DEFAULT 'user',
    language            text NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'de')),
    is_active           boolean NOT NULL DEFAULT true,
    created_at          timestamptz NOT NULL DEFAULT now(),
    -- Targets for the role-carrying foreign keys below. Role and activation
    -- changes cascade into memberships/teams, where CHECKs reject invalid
    -- states. FK row locks make this safe under any isolation level.
    UNIQUE (id, role),
    UNIQUE (id, role, is_active)
);

CREATE FUNCTION check_account_id_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id THEN
        RAISE EXCEPTION 'account ids are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER accounts_id_immutable
    BEFORE UPDATE OF id ON accounts
    FOR EACH ROW EXECUTE FUNCTION check_account_id_immutable();

CREATE TABLE teams (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name            text NOT NULL UNIQUE CHECK (btrim(name) <> ''),
    -- One team per lead account (UNIQUE); the lead holds the team_lead role and
    -- is active (FK + CHECKs) and is a member of this team (FK below). All three
    -- lead columns are NULL together, which only an archived team may do.
    lead_account_id uuid UNIQUE,
    lead_role       account_role DEFAULT 'team_lead' CHECK (lead_role = 'team_lead'),
    lead_active     boolean DEFAULT true,
    archived_at     timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (archived_at IS NOT NULL OR lead_account_id IS NOT NULL),
    CHECK (archived_at IS NOT NULL OR lead_active),
    FOREIGN KEY (lead_account_id, lead_role, lead_active)
        REFERENCES accounts (id, role, is_active) MATCH FULL ON UPDATE CASCADE
);

CREATE TABLE team_memberships (
    account_id uuid PRIMARY KEY, -- at most one team per account
    role       account_role NOT NULL CHECK (role <> 'admin'),
    team_id    uuid NOT NULL REFERENCES teams (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, team_id),
    FOREIGN KEY (account_id, role) REFERENCES accounts (id, role) ON UPDATE CASCADE
);
CREATE INDEX team_memberships_team_idx ON team_memberships (team_id);

-- The lead must be a current member of the same team. Deferred so a team, its
-- first membership and its lead are created in one transaction in any order.
ALTER TABLE teams ADD CONSTRAINT teams_lead_is_member
    FOREIGN KEY (lead_account_id, id) REFERENCES team_memberships (account_id, team_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE team_membership_history (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts (id),
    team_id    uuid NOT NULL REFERENCES teams (id),
    action     text NOT NULL CHECK (action IN ('joined', 'left')),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX team_membership_history_account_idx ON team_membership_history (account_id, created_at);
CREATE TRIGGER team_membership_history_append_only
    BEFORE UPDATE OR DELETE ON team_membership_history
    FOR EACH ROW EXECUTE FUNCTION reject_mutation();
CREATE TRIGGER team_membership_history_no_truncate
    BEFORE TRUNCATE ON team_membership_history
    FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();

CREATE FUNCTION record_team_membership() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO team_membership_history (account_id, team_id, action) VALUES (OLD.account_id, OLD.team_id, 'left');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO team_membership_history (account_id, team_id, action) VALUES (NEW.account_id, NEW.team_id, 'joined');
    END IF;
    RETURN NULL;
END $$;
-- Role cascades update only `role`, so they are not recorded as moves.
CREATE TRIGGER team_memberships_history
    AFTER INSERT OR DELETE OR UPDATE OF team_id ON team_memberships
    FOR EACH ROW EXECUTE FUNCTION record_team_membership();

CREATE TABLE categories (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL CHECK (btrim(name) <> ''),
    skip_ai     boolean NOT NULL DEFAULT false,
    sort_order  integer NOT NULL DEFAULT 0,
    archived_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE priorities (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL CHECK (btrim(name) <> ''),
    sort_order  integer NOT NULL DEFAULT 0,
    archived_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tickets (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    reference            bigint GENERATED ALWAYS AS IDENTITY UNIQUE, -- stable human-facing number
    requester_account_id uuid NOT NULL REFERENCES accounts (id),
    category_id          uuid NOT NULL REFERENCES categories (id),
    priority_id          uuid NOT NULL REFERENCES priorities (id),
    team_id              uuid REFERENCES teams (id),
    assignee_account_id  uuid,
    status               ticket_status NOT NULL DEFAULT 'draft',
    subject              text NOT NULL,
    description          text NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CHECK (assignee_account_id IS NULL OR team_id IS NOT NULL),
    CHECK (assignee_account_id IS DISTINCT FROM requester_account_id), -- no self-handling
    -- Assignees are current members of the ticket's team (so never admins).
    FOREIGN KEY (assignee_account_id, team_id) REFERENCES team_memberships (account_id, team_id)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX tickets_status_idx ON tickets (status, created_at);
CREATE INDEX tickets_team_idx ON tickets (team_id, status);
CREATE INDEX tickets_assignee_idx ON tickets (assignee_account_id) WHERE assignee_account_id IS NOT NULL;
CREATE INDEX tickets_requester_idx ON tickets (requester_account_id, created_at);

CREATE TABLE ticket_messages (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    ticket_id         uuid NOT NULL REFERENCES tickets (id),
    author_account_id uuid NOT NULL REFERENCES accounts (id),
    kind              text NOT NULL CHECK (kind IN ('public_reply', 'internal_note')),
    body              text NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (id, ticket_id)
);
CREATE INDEX ticket_messages_ticket_idx ON ticket_messages (ticket_id, created_at);

CREATE TABLE attachments (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    ticket_id           uuid NOT NULL REFERENCES tickets (id),
    message_id          uuid,
    uploader_account_id uuid NOT NULL REFERENCES accounts (id),
    filename            text NOT NULL,
    mime_type           text NOT NULL CHECK (mime_type IN ('image/png', 'image/jpeg', 'image/webp')),
    byte_size           integer NOT NULL CHECK (byte_size > 0),
    width               integer NOT NULL CHECK (width > 0),
    height              integer NOT NULL CHECK (height > 0),
    storage_key         text NOT NULL UNIQUE,
    created_at          timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (message_id, ticket_id) REFERENCES ticket_messages (id, ticket_id)
);
CREATE INDEX attachments_ticket_idx ON attachments (ticket_id);

CREATE TABLE ticket_events (
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_id        uuid NOT NULL REFERENCES tickets (id),
    actor_account_id uuid REFERENCES accounts (id), -- NULL for system jobs
    event_type       text NOT NULL,
    from_status      ticket_status,
    to_status        ticket_status,
    payload          jsonb NOT NULL DEFAULT '{}',
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ticket_events_ticket_idx ON ticket_events (ticket_id, id);
CREATE TRIGGER ticket_events_append_only
    BEFORE UPDATE OR DELETE ON ticket_events
    FOR EACH ROW EXECUTE FUNCTION reject_mutation();
CREATE TRIGGER ticket_events_no_truncate
    BEFORE TRUNCATE ON ticket_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();
