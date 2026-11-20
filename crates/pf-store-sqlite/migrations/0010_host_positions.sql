-- Migration 0010: the position an adapter has read to in its session's output (ADR-0003's
-- amendment of 2026-11-16, ADR-0007, ADR-0011's kept-and-resent event test; `r2.s2.w4`). Forward-only.
--
-- A session host keeps a session's process and output outside the controller. An adapter that reads
-- a file the session writes (OMP's event file) needs to know, after a controller restart, how far it
-- had read and what it had derived from the lines before that point. This table keeps that, one row
-- per host unit: `position` is a JSON value the adapter owns, and the store never reads inside it.
--
-- `seq` counts the saves of a unit from 1. A save is an admitted command under `@controller` with the
-- command ID `host-position-<unit>-<seq>`, so one `seq` is one admission, and a replay of a save
-- changes nothing. The port checks that the unit has a host row and that `seq` is the stored one plus
-- 1 (1 when there is none). The triggers below are the backstop of that check. No secret enters the
-- table: a position holds file offsets, counters, git SHAs and the events an adapter has taken.

CREATE TABLE host_positions (
    unit        TEXT NOT NULL PRIMARY KEY,
    seq         INTEGER NOT NULL CHECK (seq >= 1),
    position    TEXT NOT NULL CHECK (json_valid(position)),
    command_id  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

-- The one change a row allows: the next `seq` of the same unit, with a new position.
CREATE TRIGGER host_positions_next_seq BEFORE UPDATE ON host_positions
WHEN NOT (NEW.unit IS OLD.unit AND NEW.seq = OLD.seq + 1)
BEGIN SELECT RAISE(ABORT, 'a host position changes only to the next seq of the same unit'); END;

CREATE TRIGGER host_positions_never_delete BEFORE DELETE ON host_positions
BEGIN SELECT RAISE(ABORT, 'a host position is never deleted'); END;

-- `INSERT OR REPLACE` deletes the row it replaces without firing a delete trigger (`recursive_triggers`
-- is off), so it would rewrite a position past the two triggers above. This trigger refuses an insert
-- whose `unit` is in use, before SQLite resolves the conflict.
CREATE TRIGGER host_positions_never_replace BEFORE INSERT ON host_positions
WHEN EXISTS (SELECT 1 FROM host_positions WHERE unit = NEW.unit)
BEGIN SELECT RAISE(ABORT, 'a host position is never replaced'); END;
