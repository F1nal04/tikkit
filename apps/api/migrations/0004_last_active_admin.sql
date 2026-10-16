-- At least one active Admin must remain. See docs/authorization.md.
--
-- A statement-level lock serializes role/activation changes before any row is
-- locked, so two transactions demoting the last two Admins cannot deadlock;
-- the row trigger then re-reads the remaining Admins FOR UPDATE, which sees
-- the winner's commit (READ COMMITTED) or fails with 40001 (REPEATABLE READ+).
-- ponytail: one global lock for all role/activation updates; they are rare Admin actions.

CREATE FUNCTION lock_admin_set() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('tikkit.active_admins'));
    RETURN NULL;
END $$;

CREATE FUNCTION check_last_active_admin() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.role = 'admin' AND NEW.is_active THEN
        RETURN NEW;
    END IF;
    PERFORM 1 FROM accounts
        WHERE role = 'admin' AND is_active AND id <> OLD.id
        FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'the last active admin cannot be demoted or deactivated'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'last_active_admin';
    END IF;
    RETURN CASE TG_OP WHEN 'DELETE' THEN OLD ELSE NEW END;
END $$;

CREATE TRIGGER accounts_lock_admin_set
    BEFORE UPDATE OF role, is_active OR DELETE ON accounts
    FOR EACH STATEMENT EXECUTE FUNCTION lock_admin_set();

CREATE TRIGGER accounts_last_active_admin
    BEFORE UPDATE OF role, is_active OR DELETE ON accounts
    FOR EACH ROW
    WHEN (OLD.role = 'admin' AND OLD.is_active)
    EXECUTE FUNCTION check_last_active_admin();

-- Team members are staff: Users and Admins cannot join (extends the admin-only CHECK).
-- Role changes cascade here, so demoting a member to User requires leaving the team first.
ALTER TABLE team_memberships ADD CONSTRAINT team_memberships_role_staff
    CHECK (role NOT IN ('admin', 'user'));
