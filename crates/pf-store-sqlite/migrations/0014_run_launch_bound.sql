-- Migration 0014: the run's armed launch count (ADR-0003's amendment of 2026-11-16; `r2.s3.w2`).
-- Forward-only.
--
-- `arm` writes the most launches that its summary shows for the run's pinned package, once. The
-- grant's acts are bound by it: a launching `settle` that would pass it is refused. A run with no
-- count (every run armed before this migration, and a plan whose most launches are unknown) refuses
-- every launching act of the grant.
--
--   runs.launch_bound  NULL until `arm` sets it. `PutRun` never writes it. Once set, an update that
--                      changes it fails. The same value again changes nothing.
--
-- The port checks the rule before it writes. The trigger below is the backstop, as migration 0008's
-- are for a run's project: no caller can break the rule through any path. A guard trigger compares
-- with `IS NOT`, so a NULL never makes a guard pass by accident.

ALTER TABLE runs ADD COLUMN launch_bound INTEGER CHECK (launch_bound IS NULL OR launch_bound >= 0);

CREATE TRIGGER runs_launch_bound_set_once BEFORE UPDATE OF launch_bound ON runs
WHEN OLD.launch_bound IS NOT NULL AND NEW.launch_bound IS NOT OLD.launch_bound
BEGIN SELECT RAISE(ABORT, 'a run keeps its launch bound'); END;
