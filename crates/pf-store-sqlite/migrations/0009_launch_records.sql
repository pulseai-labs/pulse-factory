-- Migration 0009: launch records and the host registry (ADR-0003's amendment of 2026-11-16,
-- ADR-0007, ADR-0016; `r2.s2.w2`). Forward-only.
--
-- A launch record is what a session's next launch must use. It is written once, before the host
-- starts, and never changed: a live change that must hold after a resume writes the next revision, and
-- the newest revision wins. The record's `resume` field says how to resume; it never holds the runtime's
-- own session ID, which the runtime mints after the launch and which stays on the session row.
--
-- The host registry has one row per host. It names the record revision and hash the host was
-- launched from, so a later revision never changes what a restart compares. Its end and its exit are
-- set once, together. No secret value enters either table: a host's attach key lives in a 0600 key file
-- outside the store, so no column holds it.
--
-- No `CHECK` on `session_kind` or `launcher`: the port validates them, so a later kind or launcher
-- needs no table rebuild (the precedent of the settings key list, ADR-0016). The port checks the
-- rest: the next revision and incarnation, the secret values and the hash of the body. The triggers
-- and constraints below are the backstop of those checks.

CREATE TABLE launch_records (
    session_kind  TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    incarnation   INTEGER NOT NULL CHECK (incarnation >= 1),
    revision      INTEGER NOT NULL CHECK (revision >= 1),
    launcher      TEXT NOT NULL,
    host_unit     TEXT NOT NULL,
    host_socket   TEXT NOT NULL,
    record        TEXT NOT NULL CHECK (json_valid(record)),
    record_sha256 TEXT NOT NULL CHECK (length(record_sha256) = 64
                                       AND record_sha256 NOT GLOB '*[^0-9a-f]*'),
    command_id    TEXT NOT NULL UNIQUE,
    created_at    TEXT NOT NULL,
    PRIMARY KEY (session_kind, session_id, incarnation, revision)
) STRICT;

CREATE TRIGGER launch_records_never_update BEFORE UPDATE ON launch_records
BEGIN SELECT RAISE(ABORT, 'a launch record is write-once'); END;

CREATE TRIGGER launch_records_never_delete BEFORE DELETE ON launch_records
BEGIN SELECT RAISE(ABORT, 'a launch record is write-once'); END;

CREATE TABLE hosts (
    unit                  TEXT NOT NULL PRIMARY KEY,
    session_kind          TEXT NOT NULL,
    session_id            TEXT NOT NULL,
    incarnation           INTEGER NOT NULL CHECK (incarnation >= 1),
    launched_revision     INTEGER NOT NULL CHECK (launched_revision >= 1),
    launched_record_sha256 TEXT NOT NULL CHECK (length(launched_record_sha256) = 64
                                                AND launched_record_sha256 NOT GLOB '*[^0-9a-f]*'),
    launcher              TEXT NOT NULL,
    socket                TEXT NOT NULL,
    invocation_id         TEXT NOT NULL,
    host_binary           TEXT NOT NULL,
    host_sha256           TEXT NOT NULL,
    protocol              INTEGER NOT NULL CHECK (protocol >= 0),
    started_at            TEXT NOT NULL,
    ended_at              TEXT,
    exit                  TEXT CHECK (exit IS NULL OR json_valid(exit)),
    command_id            TEXT NOT NULL UNIQUE,
    CHECK ((ended_at IS NULL) = (exit IS NULL))
) STRICT;

-- One incarnation is one launch, so it has one host.
CREATE UNIQUE INDEX hosts_one_per_incarnation ON hosts (session_kind, session_id, incarnation);

CREATE INDEX hosts_live ON hosts (unit) WHERE ended_at IS NULL;

-- The one change a host row allows: the null `ended_at` and `exit` set once, together, with every
-- other column as it was.
CREATE TRIGGER hosts_end_once BEFORE UPDATE ON hosts
WHEN NOT (OLD.ended_at IS NULL AND OLD.exit IS NULL
          AND NEW.ended_at IS NOT NULL AND NEW.exit IS NOT NULL
          AND NEW.unit IS OLD.unit
          AND NEW.session_kind IS OLD.session_kind
          AND NEW.session_id IS OLD.session_id
          AND NEW.incarnation IS OLD.incarnation
          AND NEW.launched_revision IS OLD.launched_revision
          AND NEW.launched_record_sha256 IS OLD.launched_record_sha256
          AND NEW.launcher IS OLD.launcher
          AND NEW.socket IS OLD.socket
          AND NEW.invocation_id IS OLD.invocation_id
          AND NEW.host_binary IS OLD.host_binary
          AND NEW.host_sha256 IS OLD.host_sha256
          AND NEW.protocol IS OLD.protocol
          AND NEW.started_at IS OLD.started_at
          AND NEW.command_id IS OLD.command_id)
BEGIN SELECT RAISE(ABORT, 'a host row changes only when it ends, once, with its exit'); END;

CREATE TRIGGER hosts_never_delete BEFORE DELETE ON hosts
BEGIN SELECT RAISE(ABORT, 'a host row is never deleted'); END;
