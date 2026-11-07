-- Migration 0003: projects (ADR-0003, ADR-0007). Forward-only.
-- A project is a controller entity: an id, a name and one root. Nothing else is decided yet (the
-- project hierarchy has its own brainstorm), so the table holds no repo, pairing or other field.

CREATE TABLE projects (
    id          TEXT PRIMARY KEY,            -- P-<n>, never reused
    name        TEXT NOT NULL,
    root        TEXT NOT NULL,               -- absolute, canonical
    created_at  TEXT NOT NULL,
    removed_at  TEXT                         -- set by project remove; the row stays
) STRICT;
CREATE UNIQUE INDEX projects_live_root ON projects (root) WHERE removed_at IS NULL;
CREATE UNIQUE INDEX projects_live_name ON projects (name) WHERE removed_at IS NULL;
-- The admission scope "@controller" (Approach 3) can never be a run id.
CREATE TRIGGER runs_id_has_no_at BEFORE INSERT ON runs WHEN instr(NEW.id, '@') > 0
BEGIN SELECT RAISE(ABORT, 'a run id cannot contain @'); END;
