-- Migration 0002: requests and claims (ADR-0006, ADR-0009). Forward-only.
-- Every halt opens a request. A claim lets one session answer; a new claim fences the old one.

-- One row per request. The ID is `R-<n>` from one sequence for the whole store. A run-level
-- request has no unit.
CREATE TABLE requests (
    id                 TEXT PRIMARY KEY CHECK (id GLOB 'R-[0-9]*'),
    revision           INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    run_id             TEXT NOT NULL,
    unit_id            TEXT,
    stage              TEXT NOT NULL DEFAULT '',
    kind               TEXT NOT NULL CHECK (
                           kind IN ('park', 'gaps', 'stop', 'red_gate', 'stall', 'merge_failed',
                                    'seat_lost', 'merge_ack')),
    reason             TEXT NOT NULL,
    body               TEXT NOT NULL DEFAULT '{}',
    status             TEXT NOT NULL CHECK (status IN ('open', 'answered', 'withdrawn')),
    opened_at          TEXT NOT NULL,
    answered_at        TEXT,
    answer             TEXT,
    answer_generation  INTEGER
) STRICT;

-- A unit has at most one open request.
CREATE UNIQUE INDEX requests_one_open_per_unit
    ON requests (run_id, unit_id) WHERE status = 'open' AND unit_id IS NOT NULL;
CREATE INDEX requests_by_run ON requests (run_id, status);

-- One row per claim. The current claim is the highest generation with no `fenced_at`. The token
-- itself is never stored: only its SHA-256 as 64 hex digits.
CREATE TABLE claims (
    generation  INTEGER PRIMARY KEY CHECK (generation >= 1),
    token_hash  TEXT NOT NULL CHECK (length(token_hash) = 64),
    holder      TEXT NOT NULL,
    claimed_at  TEXT NOT NULL,
    fenced_at   TEXT
) STRICT;

-- At most one claim is current.
CREATE UNIQUE INDEX claims_one_current ON claims ((fenced_at IS NULL)) WHERE fenced_at IS NULL;
