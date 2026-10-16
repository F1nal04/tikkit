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
    username            text NOT NULL CHECK (btrim(username) <> ''),
    username_normalized text GENERATED ALWAYS AS (lower(btrim(username))) STORED UNIQUE,
    display_name        text NOT NULL,
    email               text NOT NULL, -- contact only, deliberately not unique
    password_hash       text NOT NULL,
    role                account_role NOT NULL DEFAULT 'user',
    language            text NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'de')),
    is_active           boolean NOT NULL DEFAULT true,
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE teams (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name           text NOT NULL UNIQUE CHECK (btrim(name) <> ''),
    -- One team per lead account; a lead must be a member of this team (FK below).
    lead_account_id uuid UNIQUE,
    archived_at    timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (archived_at IS NOT NULL OR lead_account_id IS NOT NULL)
);

CREATE TABLE team_memberships (
    account_id uuid PRIMARY KEY REFERENCES accounts (id), -- at most one team per account
    team_id    uuid NOT NULL REFERENCES teams (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, team_id)
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

-- Row locks (FOR SHARE) serialise these checks against concurrent role changes.
CREATE FUNCTION check_team_membership() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        INSERT INTO team_membership_history (account_id, team_id, action) VALUES (OLD.account_id, OLD.team_id, 'left');
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        IF (SELECT role FROM accounts WHERE id = NEW.account_id FOR SHARE) = 'admin' THEN
            RAISE EXCEPTION 'admin accounts cannot be team members' USING ERRCODE = 'check_violation';
        END IF;
        INSERT INTO team_membership_history (account_id, team_id, action) VALUES (NEW.account_id, NEW.team_id, 'joined');
        RETURN NEW;
    END IF;
    RETURN OLD;
END $$;
CREATE TRIGGER team_memberships_check
    BEFORE INSERT OR UPDATE OR DELETE ON team_memberships
    FOR EACH ROW EXECUTE FUNCTION check_team_membership();

CREATE FUNCTION check_team_lead() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.lead_account_id IS NOT NULL
       AND (SELECT role FROM accounts WHERE id = NEW.lead_account_id FOR SHARE) IS DISTINCT FROM 'team_lead' THEN
        RAISE EXCEPTION 'team lead must have the team_lead role' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER teams_lead_check
    BEFORE INSERT OR UPDATE OF lead_account_id ON teams
    FOR EACH ROW EXECUTE FUNCTION check_team_lead();

CREATE FUNCTION check_account_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id THEN
        RAISE EXCEPTION 'account ids are immutable' USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.role = 'admin' AND EXISTS (SELECT 1 FROM team_memberships WHERE account_id = NEW.id) THEN
        RAISE EXCEPTION 'team members cannot become admins' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.role <> 'team_lead' AND EXISTS (SELECT 1 FROM teams WHERE lead_account_id = NEW.id) THEN
        RAISE EXCEPTION 'a current team lead must keep the team_lead role' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER accounts_update_check
    BEFORE UPDATE ON accounts
    FOR EACH ROW EXECUTE FUNCTION check_account_update();

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
