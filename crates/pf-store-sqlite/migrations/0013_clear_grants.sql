-- Migration 0013: the clear grant (ADR-0003's amendment of 2026-11-16, ADR-0005's and ADR-0009's;
-- `r2.s3.w1`). Forward-only.
--
-- A clear grant is one row for each project and generation: the agent that holds it, who gave it
-- and when, and, once the operator ends it, who ended it and when. The grant is off until the
-- operator gives it, and the operator can end it at any time. A revoke ends its generation, and the
-- next grant takes the next one, so an act under an old generation is refused.
--
--   clear_grants  one row per `(project_id, generation)`. A new row's generation is the project's
--                 highest plus 1, from 1. At most one row of a project has no revoke. An update may
--                 only set `revoked_by` and `revoked_at` together, once. A row is never deleted.
--
-- Every change is an admitted command under `@controller`, and the port checks each rule before it
-- writes. The index and the triggers below are the backstop, as migration 0012's are for the
-- notices: no caller can break a rule through any path. A guard trigger compares with `IS` and
-- `IS NOT`, so a NULL never makes a guard pass by accident. No secret enters the table.

CREATE TABLE clear_grants (
    project_id   TEXT NOT NULL,       -- a project's ID, `P-<n>`; the project row may be removed later
    generation   INTEGER NOT NULL CHECK (generation >= 1),
    agent_id     TEXT NOT NULL,       -- opaque to the store; the controller names it
    granted_by   TEXT NOT NULL,
    granted_at   TEXT NOT NULL,
    revoked_by   TEXT,                -- null until the revoke
    revoked_at   TEXT,                -- null until the revoke
    command_id   TEXT NOT NULL UNIQUE,
    PRIMARY KEY (project_id, generation),
    CHECK ((revoked_by IS NULL) = (revoked_at IS NULL))
) STRICT;

-- At most one row of a project has no revoke.
CREATE UNIQUE INDEX clear_grants_one_live ON clear_grants (project_id) WHERE revoked_at IS NULL;

-- A new row's generation is the project's highest plus 1 (1 for a project with none), and a row is
-- never born revoked.
CREATE TRIGGER clear_grants_next_generation BEFORE INSERT ON clear_grants
WHEN NEW.generation IS NOT COALESCE(
        (SELECT MAX(generation) FROM clear_grants WHERE project_id IS NEW.project_id), 0) + 1
BEGIN SELECT RAISE(ABORT, 'a grant takes the next generation of its project'); END;

CREATE TRIGGER clear_grants_born_live BEFORE INSERT ON clear_grants
WHEN NEW.revoked_by IS NOT NULL OR NEW.revoked_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a grant is born live'); END;

-- `INSERT OR REPLACE` deletes the row it replaces without firing a delete trigger
-- (`recursive_triggers` is off), so it would rewrite a grant past the triggers here. This trigger
-- refuses an insert whose key or command ID is in use, before SQLite resolves the conflict.
CREATE TRIGGER clear_grants_never_replace BEFORE INSERT ON clear_grants
WHEN EXISTS (
    SELECT 1 FROM clear_grants
    WHERE (project_id IS NEW.project_id AND generation IS NEW.generation)
       OR command_id IS NEW.command_id
)
BEGIN SELECT RAISE(ABORT, 'a grant is never replaced'); END;

-- The one change a row allows: the revoke, both columns together, once, on a row with none.
CREATE TRIGGER clear_grants_revoke_once BEFORE UPDATE ON clear_grants
WHEN NOT (
    OLD.revoked_by IS NULL AND OLD.revoked_at IS NULL
    AND NEW.revoked_by IS NOT NULL AND NEW.revoked_at IS NOT NULL
    AND NEW.project_id IS OLD.project_id AND NEW.generation IS OLD.generation
    AND NEW.agent_id IS OLD.agent_id AND NEW.granted_by IS OLD.granted_by
    AND NEW.granted_at IS OLD.granted_at AND NEW.command_id IS OLD.command_id
)
BEGIN SELECT RAISE(ABORT, 'a grant changes only by one revoke of both columns'); END;

CREATE TRIGGER clear_grants_never_delete BEFORE DELETE ON clear_grants
BEGIN SELECT RAISE(ABORT, 'a grant is never deleted'); END;
