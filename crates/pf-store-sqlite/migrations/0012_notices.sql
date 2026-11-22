-- Migration 0012: the notices (ADR-0003's amendment of 2026-11-21, ADR-0006's, ADR-0007; `r2.s11.w3`).
-- Forward-only.
--
-- A notice is the controller's record of one request event: a request opened, was revised, or was
-- closed. It is derived from the request and never changes. Each notice has one delivery for each
-- channel, one acknowledged position for each target, and one send count for each notice and
-- target. The store keeps a channel and a target as opaque text; the controller names them.
--
--   notices            one row per `(request_id, revision, event)`. `position` is one sequence for the
--                      store, from 1. A row never changes and is never deleted.
--   notice_deliveries  one row per notice and channel: the text the controller wrote and whether it
--                      still `waiting` or is `superseded` (with the `why`). The one change a row
--                      allows is `waiting` to `superseded`.
--   notice_targets     one row per target: `acked` is the highest position it acknowledged, and it
--                      only rises; `outcome` is the last acknowledgement's.
--   notice_sends       one row per notice and target: the count of sends, which rises by 1, and the
--                      time of the last one.
--
-- Every change is an admitted command under `@controller`, and the port checks each rule before it
-- writes. The triggers below are the backstop, as migration 0010's are for the host positions. No
-- secret enters a table: a text is controller-made and redacted before it is stored.

CREATE TABLE notices (
    position    INTEGER PRIMARY KEY,
    request_id  TEXT NOT NULL,
    revision    INTEGER NOT NULL CHECK (revision >= 1),
    event       TEXT NOT NULL CHECK (event IN ('opened', 'revised', 'closed')),
    run_id      TEXT NOT NULL,
    project_id  TEXT,                 -- null for a run with no project
    created_at  TEXT NOT NULL,
    command_id  TEXT NOT NULL,
    UNIQUE (request_id, revision, event)
) STRICT;

CREATE TRIGGER notices_never_update BEFORE UPDATE ON notices
BEGIN SELECT RAISE(ABORT, 'a notice never changes'); END;

CREATE TRIGGER notices_never_delete BEFORE DELETE ON notices
BEGIN SELECT RAISE(ABORT, 'a notice is never deleted'); END;

-- `INSERT OR REPLACE` deletes the row it replaces without firing a delete trigger (`recursive_triggers`
-- is off), so it would rewrite a notice past the two triggers above. This trigger refuses an insert
-- whose position, or whose request, revision and event, are in use, before SQLite resolves the conflict.
CREATE TRIGGER notices_never_replace BEFORE INSERT ON notices
WHEN EXISTS (
    SELECT 1 FROM notices
    WHERE position = NEW.position
       OR (request_id = NEW.request_id AND revision = NEW.revision AND event = NEW.event)
)
BEGIN SELECT RAISE(ABORT, 'a notice is never replaced'); END;

CREATE TABLE notice_deliveries (
    position    INTEGER NOT NULL,
    channel     TEXT NOT NULL CHECK (length(channel) > 0),
    text        TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('waiting', 'superseded')),
    why         TEXT,                 -- null while the delivery waits
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (position, channel),
    CHECK ((state = 'waiting' AND why IS NULL) OR (state = 'superseded' AND why IS NOT NULL))
) STRICT;

-- The one change a row allows: `waiting` to `superseded`, with a reason and a time.
CREATE TRIGGER notice_deliveries_supersede_only BEFORE UPDATE ON notice_deliveries
WHEN NOT (
    OLD.state = 'waiting' AND NEW.state = 'superseded'
    AND NEW.position = OLD.position AND NEW.channel = OLD.channel AND NEW.text = OLD.text
)
BEGIN SELECT RAISE(ABORT, 'a delivery changes only from waiting to superseded'); END;

CREATE TRIGGER notice_deliveries_never_delete BEFORE DELETE ON notice_deliveries
BEGIN SELECT RAISE(ABORT, 'a delivery is never deleted'); END;

CREATE TRIGGER notice_deliveries_never_replace BEFORE INSERT ON notice_deliveries
WHEN EXISTS (
    SELECT 1 FROM notice_deliveries WHERE position = NEW.position AND channel = NEW.channel
)
BEGIN SELECT RAISE(ABORT, 'a delivery is never replaced'); END;

CREATE TABLE notice_targets (
    target      TEXT NOT NULL PRIMARY KEY,
    channel     TEXT NOT NULL CHECK (length(channel) > 0),
    acked       INTEGER NOT NULL CHECK (acked >= 0),
    outcome     TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

-- The one change a row allows: a higher `acked` of the same target and channel.
CREATE TRIGGER notice_targets_rise_only BEFORE UPDATE ON notice_targets
WHEN NOT (
    NEW.target = OLD.target AND NEW.channel = OLD.channel AND NEW.acked > OLD.acked
)
BEGIN SELECT RAISE(ABORT, 'the position of a target only rises'); END;

CREATE TRIGGER notice_targets_never_delete BEFORE DELETE ON notice_targets
BEGIN SELECT RAISE(ABORT, 'a target is never deleted'); END;

CREATE TRIGGER notice_targets_never_replace BEFORE INSERT ON notice_targets
WHEN EXISTS (SELECT 1 FROM notice_targets WHERE target = NEW.target)
BEGIN SELECT RAISE(ABORT, 'a target is never replaced'); END;

CREATE TABLE notice_sends (
    position       INTEGER NOT NULL,
    target         TEXT NOT NULL,
    sends          INTEGER NOT NULL CHECK (sends >= 1),
    last_sent_at   TEXT NOT NULL,
    PRIMARY KEY (position, target)
) STRICT;

-- The one change a row allows: the next count of the same notice and target.
CREATE TRIGGER notice_sends_next_only BEFORE UPDATE ON notice_sends
WHEN NOT (
    NEW.position = OLD.position AND NEW.target = OLD.target AND NEW.sends = OLD.sends + 1
)
BEGIN SELECT RAISE(ABORT, 'a send count changes only to the next count of the same notice and target'); END;

CREATE TRIGGER notice_sends_never_delete BEFORE DELETE ON notice_sends
BEGIN SELECT RAISE(ABORT, 'a send row is never deleted'); END;

-- A first send starts at 1. A row is never replaced.
CREATE TRIGGER notice_sends_start_at_one BEFORE INSERT ON notice_sends
WHEN NEW.sends <> 1
BEGIN SELECT RAISE(ABORT, 'a first send starts at 1'); END;

CREATE TRIGGER notice_sends_never_replace BEFORE INSERT ON notice_sends
WHEN EXISTS (SELECT 1 FROM notice_sends WHERE position = NEW.position AND target = NEW.target)
BEGIN SELECT RAISE(ABORT, 'a send row is never replaced'); END;
