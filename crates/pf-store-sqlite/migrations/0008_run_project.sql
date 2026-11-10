-- Migration 0008: a run belongs to a project when a device armed it from a chat's start card
-- (ADR-0003, ADR-0007, ADR-0015; `r1.s4.w2`). Forward-only.
--
-- The column is nullable. Every existing row keeps NULL, and so does every run that the CLI arms.
-- `put_run` upserts by naming its columns and never names this one, so an upsert never clears or
-- changes it. Only the admitted effect `SetRunProject` writes it. The triggers are the backstop of
-- that effect's checks: a run names a live project, a named project is never changed or cleared, and
-- the project is a row of `projects`.

ALTER TABLE runs ADD COLUMN project_id TEXT;

CREATE TRIGGER runs_project_names_a_project BEFORE INSERT ON runs
WHEN NEW.project_id IS NOT NULL
 AND NOT EXISTS (SELECT 1 FROM projects WHERE id = NEW.project_id)
BEGIN SELECT RAISE(ABORT, 'a run names a project that exists'); END;

CREATE TRIGGER runs_project_is_live_on_insert BEFORE INSERT ON runs
WHEN NEW.project_id IS NOT NULL
 AND NOT EXISTS (SELECT 1 FROM projects WHERE id = NEW.project_id AND removed_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'a run names a live project'); END;

CREATE TRIGGER runs_project_is_live BEFORE UPDATE OF project_id ON runs
WHEN NEW.project_id IS NOT NULL
 AND NOT EXISTS (SELECT 1 FROM projects WHERE id = NEW.project_id AND removed_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'a run names a live project'); END;

CREATE TRIGGER runs_project_never_changes BEFORE UPDATE OF project_id ON runs
WHEN OLD.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
BEGIN SELECT RAISE(ABORT, 'a run keeps its project'); END;
