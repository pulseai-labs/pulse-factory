-- Migration 0007: scoped settings (ADR-0003, ADR-0007, ADR-0016). Forward-only.
--
-- One row is one value of one key at one scope. '' in a scope column means "every": both '' is the
-- server scope. NULL would not do: SQLite treats NULLs as distinct in a primary key, so two server
-- rows of one key could coexist. The port maps '' to None both ways. Rows change only through
-- admitted commands (setting_set, setting_unset). The table takes any key of the grammar: the
-- controller's registry decides which keys exist, so a later key needs no table rebuild. A removed
-- project keeps its rows; they never apply again, because a re-added root gets a new P-<n>.

CREATE TABLE settings (
    project_id  TEXT NOT NULL DEFAULT '',     -- '' or a P-<n>
    agent_id    TEXT NOT NULL DEFAULT '',     -- '' or an agents.id
    key         TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 64
                                     AND key NOT GLOB '*[^a-z0-9_.]*'),
    value       TEXT NOT NULL CHECK (json_valid(value)),
    set_by      TEXT NOT NULL CHECK (set_by = 'cli' OR set_by GLOB 'D-[0-9]*'),
    set_at      TEXT NOT NULL,
    PRIMARY KEY (project_id, agent_id, key)
) STRICT;

CREATE TRIGGER settings_project_is_live BEFORE INSERT ON settings
WHEN NEW.project_id <> '' AND NOT EXISTS
     (SELECT 1 FROM projects WHERE id = NEW.project_id AND removed_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'a setting names a live project'); END;

CREATE TRIGGER settings_agent_exists BEFORE INSERT ON settings
WHEN NEW.agent_id <> '' AND NOT EXISTS (SELECT 1 FROM agents WHERE id = NEW.agent_id)
BEGIN SELECT RAISE(ABORT, 'a setting names a known agent'); END;

CREATE TRIGGER settings_scope_never_changes BEFORE UPDATE OF project_id, agent_id, key ON settings
BEGIN SELECT RAISE(ABORT, 'a setting keeps its scope and key'); END;
