-- Ticket catalog: categories and priorities (from 0001) plus limited conditional
-- intake fields. Nothing here is deleted: archived rows leave new-ticket forms
-- but stay referenced by existing tickets. See docs/schema.md.

-- Names are unique among active rows only, so an archived name can be reused.
CREATE UNIQUE INDEX categories_active_name_key ON categories (lower(btrim(name))) WHERE archived_at IS NULL;
CREATE UNIQUE INDEX priorities_active_name_key ON priorities (lower(btrim(name))) WHERE archived_at IS NULL;

-- A contextual field shown on the shared form while one of its categories is selected.
CREATE TABLE intake_fields (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    label       text NOT NULL CHECK (btrim(label) <> ''),
    guidance    text NOT NULL DEFAULT '',
    kind        text NOT NULL CHECK (kind IN ('text', 'select')),
    required    boolean NOT NULL DEFAULT false, -- applies only while visible
    allow_other boolean NOT NULL DEFAULT false, -- free-text alternative to the choices
    sort_order  integer NOT NULL DEFAULT 0,
    archived_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (kind = 'select' OR NOT allow_other)
);

-- Triggering selections: the categories that reveal a field.
CREATE TABLE intake_field_categories (
    field_id    uuid NOT NULL REFERENCES intake_fields (id),
    category_id uuid NOT NULL REFERENCES categories (id),
    PRIMARY KEY (field_id, category_id)
);
CREATE INDEX intake_field_categories_category_idx ON intake_field_categories (category_id);

-- Stable choice IDs: labels may change and choices may be archived, never deleted.
CREATE TABLE intake_field_choices (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    field_id    uuid NOT NULL REFERENCES intake_fields (id),
    label       text NOT NULL CHECK (btrim(label) <> ''),
    sort_order  integer NOT NULL DEFAULT 0,
    archived_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX intake_field_choices_field_idx ON intake_field_choices (field_id, sort_order);
