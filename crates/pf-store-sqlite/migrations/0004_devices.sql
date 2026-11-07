-- Migration 0004: devices (ADR-0003, ADR-0007, ADR-0012). Forward-only.
-- A device is a controller entity: a client paired with a one-use code that holds a revocable
-- token. The store keeps the SHA-256 of the token and never the token. A revoked row stays, so its
-- ID and its token hash are never used again.

CREATE TABLE devices (
    id            TEXT PRIMARY KEY,   -- D-<n>, never reused
    name          TEXT NOT NULL,
    token_sha256  TEXT NOT NULL UNIQUE,
    paired_at     TEXT NOT NULL,
    last_seen_at  TEXT,               -- telemetry: written outside admission, once a minute at most
    last_peer     TEXT,
    revoked_at    TEXT                -- set by device revoke; the row stays
) STRICT;
