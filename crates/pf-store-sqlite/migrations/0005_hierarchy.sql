-- Migration 0005: the project hierarchy (ADR-0003, ADR-0007, ADR-0015). Forward-only.
--
-- server -> project -> worktree -> chat session. A project gains its kind and remote key. The
-- worktrees, the agents (with the built-in Orchestrator), the chat sessions and the chat items (the
-- display log of a chat) are new. The kind of a project that exists is set by the controller at its
-- next start: a migration cannot run git.

ALTER TABLE projects ADD COLUMN kind TEXT CHECK (kind IS NULL OR kind IN ('git', 'folder'));
ALTER TABLE projects ADD COLUMN remote_key TEXT;     -- host/path of the git remote; NULL: none
-- A new project always has a kind; a kind, once set, never changes (design 3.1). A project that
-- existed before this migration has none until the backfill sets it once.
CREATE TRIGGER projects_new_has_kind BEFORE INSERT ON projects WHEN NEW.kind IS NULL
BEGIN SELECT RAISE(ABORT, 'a new project has a kind'); END;
CREATE TRIGGER projects_kind_never_changes BEFORE UPDATE OF kind ON projects
WHEN OLD.kind IS NOT NULL AND NEW.kind IS NOT OLD.kind
BEGIN SELECT RAISE(ABORT, 'a project kind never changes'); END;

CREATE TABLE worktrees (
    id          TEXT PRIMARY KEY,              -- W-<n>, never reused
    project_id  TEXT NOT NULL,
    path        TEXT NOT NULL,                 -- absolute, canonical
    created_by  TEXT NOT NULL CHECK (created_by IN ('root', 'pulse-factory', 'external')),
    created_at  TEXT NOT NULL,
    gone_at     TEXT,                          -- set by the first read that saw it gone
    removed_at  TEXT                           -- set with its project's remove
) STRICT;
CREATE UNIQUE INDEX worktrees_live_path ON worktrees (path)
    WHERE gone_at IS NULL AND removed_at IS NULL;
CREATE TRIGGER worktrees_project_is_live BEFORE INSERT ON worktrees
WHEN NOT EXISTS (SELECT 1 FROM projects WHERE id = NEW.project_id AND removed_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'a worktree belongs to a live project'); END;
CREATE TRIGGER worktrees_gone_is_final BEFORE UPDATE OF gone_at ON worktrees
WHEN OLD.gone_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a gone worktree stays gone'); END;

CREATE TABLE agents (
    id               TEXT PRIMARY KEY,         -- 'orchestrator' for the built-in
    name             TEXT NOT NULL,
    title            TEXT NOT NULL,
    built_in         INTEGER NOT NULL CHECK (built_in IN (0, 1)),
    colour           TEXT NOT NULL,
    head             TEXT NOT NULL,
    instructions     TEXT,                     -- NULL for a built-in: the binary's own text
    runtime          TEXT NOT NULL,            -- opaque: a chat runtime's id
    model            TEXT NOT NULL,
    effort           TEXT NOT NULL,
    permission_mode  TEXT NOT NULL,
    skills           TEXT NOT NULL CHECK (json_valid(skills) AND json_type(skills) = 'array'),
    runtime_options  TEXT NOT NULL
                     CHECK (json_valid(runtime_options) AND json_type(runtime_options) = 'object'),
    launch_env       TEXT NOT NULL
                     CHECK (json_valid(launch_env) AND json_type(launch_env) = 'object'),
    created_at       TEXT NOT NULL
) STRICT;
INSERT INTO agents VALUES ('orchestrator', 'Orchestrator', 'Runs this project', 1, 'accent',
    'orchestrator', NULL, 'claude-code', 'claude-opus-5-5', 'high', 'bypassPermissions', '[]',
    '{"artifacts":"on"}', '{}', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE chat_sessions (
    id                   TEXT PRIMARY KEY,     -- S-<n>, never reused
    project_id           TEXT NOT NULL,        -- repeats the worktree's project, for the index
    worktree_id          TEXT NOT NULL,
    agent_id             TEXT NOT NULL,
    runtime              TEXT NOT NULL,        -- copied from the agent at start; opaque
    runtime_session_ref  TEXT,                 -- the CLI's own session id; NULL until reported
    state                TEXT NOT NULL CHECK (state IN ('working', 'needs_you', 'idle')),
    model                TEXT NOT NULL,        -- current; starts from the agent's default (P3)
    effort               TEXT NOT NULL,
    permission_mode      TEXT NOT NULL,
    input_tokens         INTEGER NOT NULL DEFAULT 0,
    output_tokens        INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens    INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_micros          INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    ended_at             TEXT                  -- live means NULL; the only record of an end
) STRICT;
CREATE UNIQUE INDEX chat_sessions_one_live_orchestrator ON chat_sessions (project_id)
    WHERE agent_id = 'orchestrator' AND ended_at IS NULL;
CREATE TRIGGER chat_sessions_project_is_the_worktrees BEFORE INSERT ON chat_sessions
WHEN NOT EXISTS (SELECT 1 FROM worktrees WHERE id = NEW.worktree_id AND project_id = NEW.project_id)
BEGIN SELECT RAISE(ABORT, 'a chat session''s project is its worktree''s project'); END;
CREATE TRIGGER chat_sessions_worktree_is_live BEFORE INSERT ON chat_sessions
WHEN EXISTS (SELECT 1 FROM worktrees WHERE id = NEW.worktree_id
             AND (gone_at IS NOT NULL OR removed_at IS NOT NULL))
BEGIN SELECT RAISE(ABORT, 'a chat session cannot start in a gone or removed worktree'); END;
CREATE TRIGGER chat_sessions_owner_never_changes
BEFORE UPDATE OF project_id, worktree_id, agent_id ON chat_sessions
BEGIN SELECT RAISE(ABORT, 'a chat session keeps its project, worktree and agent'); END;
CREATE TRIGGER chat_sessions_end_is_final BEFORE UPDATE OF ended_at ON chat_sessions
WHEN OLD.ended_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'an ended chat session stays ended'); END;

CREATE TABLE chat_items (
    session_id             TEXT NOT NULL,
    seq                    INTEGER NOT NULL,   -- from 1 within the session
    kind                   TEXT NOT NULL,      -- the port's closed list (no CHECK: a later kind needs no rebuild)
    payload                TEXT NOT NULL CHECK (json_valid(payload)),
    tool_input             TEXT CHECK (tool_input IS NULL
                                       OR length(CAST(tool_input AS BLOB)) <= 16384),
    tool_input_bytes       INTEGER,            -- the input's size before the cut
    tool_input_truncated   INTEGER NOT NULL DEFAULT 0 CHECK (tool_input_truncated IN (0, 1)),
    tool_output            TEXT CHECK (tool_output IS NULL
                                       OR length(CAST(tool_output AS BLOB)) <= 16384),
    tool_output_bytes      INTEGER,            -- the output's size before the cut
    tool_output_truncated  INTEGER NOT NULL DEFAULT 0 CHECK (tool_output_truncated IN (0, 1)),
    answers_seq            INTEGER,            -- an answer item: the seq of the request it answers
    redacted_at            TEXT,               -- set once by the admitted redaction (C12)
    created_at             TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT;
-- One answer per request (C2).
CREATE UNIQUE INDEX chat_items_one_answer ON chat_items (session_id, answers_seq)
    WHERE answers_seq IS NOT NULL;
CREATE TRIGGER chat_items_session_exists BEFORE INSERT ON chat_items
WHEN NOT EXISTS (SELECT 1 FROM chat_sessions WHERE id = NEW.session_id)
BEGIN SELECT RAISE(ABORT, 'a chat item belongs to a chat session'); END;
CREATE TRIGGER chat_items_answer_names_an_earlier_item BEFORE INSERT ON chat_items
WHEN NEW.answers_seq IS NOT NULL AND NOT EXISTS (SELECT 1 FROM chat_items
     WHERE session_id = NEW.session_id AND seq = NEW.answers_seq AND seq < NEW.seq)
BEGIN SELECT RAISE(ABORT, 'an answer names an earlier item of its session'); END;
-- Append-only, except one redaction (C12): redacted_at goes from NULL to a value, the text columns
-- become the fixed marks (a column that holds no text stays NULL), and every other column stays as
-- it was. Every test uses IS, so a NULL gives a plain false and the trigger fires. The marks are REDACTED_PAYLOAD and REDACTED_TEXT of the store port.
CREATE TRIGGER chat_items_change_only_by_redaction BEFORE UPDATE ON chat_items
WHEN NOT (
    OLD.redacted_at IS NULL AND NEW.redacted_at IS NOT NULL
    AND NEW.session_id IS OLD.session_id AND NEW.seq IS OLD.seq AND NEW.kind IS OLD.kind
    AND NEW.answers_seq IS OLD.answers_seq AND NEW.created_at IS OLD.created_at
    AND NEW.tool_input_bytes IS OLD.tool_input_bytes
    AND NEW.tool_input_truncated IS OLD.tool_input_truncated
    AND NEW.tool_output_bytes IS OLD.tool_output_bytes
    AND NEW.tool_output_truncated IS OLD.tool_output_truncated
    AND NEW.payload IS '{"redacted":true}'
    AND ((OLD.tool_input IS NULL AND NEW.tool_input IS NULL)
         OR (OLD.tool_input IS NOT NULL AND NEW.tool_input IS '[redacted]'))
    AND ((OLD.tool_output IS NULL AND NEW.tool_output IS NULL)
         OR (OLD.tool_output IS NOT NULL AND NEW.tool_output IS '[redacted]')))
BEGIN SELECT RAISE(ABORT, 'a chat item changes only by one redaction'); END;
CREATE TRIGGER chat_items_are_never_deleted BEFORE DELETE ON chat_items
BEGIN SELECT RAISE(ABORT, 'a chat item is never deleted'); END;
