-- Migration 0001: only what skeleton A needs.
-- Requests and claims come in a later migration. Projects, agents and devices come with the
-- schema design of the release after Release 0.

CREATE TABLE runs (
    id             TEXT PRIMARY KEY,
    workflow       TEXT NOT NULL,
    ledger_file    TEXT NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('active', 'ended')),
    halted_reason  TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
) STRICT;

-- The tree: one node per unit, and one per stage attempt under it.
CREATE TABLE nodes (
    run_id      TEXT NOT NULL,
    id          TEXT NOT NULL,
    parent_id   TEXT,
    kind        TEXT NOT NULL CHECK (kind IN ('unit', 'stage')),
    status      TEXT NOT NULL CHECK (
                    status IN ('running', 'done', 'failed', 'stopped', 'waiting_on_you', 'parked')),
    reason      TEXT,
    role        TEXT,
    model       TEXT,
    attempt     INTEGER NOT NULL DEFAULT 0,
    detail      TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (run_id, id)
) STRICT;

-- Token usage by kind and dollar cost (millionths of a dollar) of each model session.
CREATE TABLE sessions (
    run_id              TEXT NOT NULL,
    id                  TEXT NOT NULL,
    node_id             TEXT NOT NULL,
    unit_id             TEXT NOT NULL,
    process_ref         TEXT,
    ended               INTEGER NOT NULL DEFAULT 0,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    cost_micros         INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    PRIMARY KEY (run_id, id)
) STRICT;

-- One row per command. The command ID is a unique key, and the content bytes are compared.
CREATE TABLE admissions (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id       TEXT NOT NULL,
    command_id   TEXT NOT NULL,
    kind         TEXT NOT NULL,
    content      TEXT NOT NULL,
    outcome      TEXT NOT NULL CHECK (outcome IN ('admitted', 'refused')),
    reason       TEXT,
    unit_id      TEXT,
    role         TEXT,
    model        TEXT,
    attempt      INTEGER,
    admitted_at  TEXT NOT NULL,
    UNIQUE (run_id, command_id)
) STRICT;

-- The ledger outbox: a line is committed here with its transition, then appended to the file.
CREATE TABLE ledger_outbox (
    ledger_file    TEXT NOT NULL,
    seq            INTEGER NOT NULL,
    run_id         TEXT NOT NULL,
    admission_seq  INTEGER NOT NULL,
    body           TEXT NOT NULL,
    written        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (ledger_file, seq)
) STRICT;

CREATE TABLE schema_version (
    version     INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    applied_at  TEXT NOT NULL
) STRICT;
