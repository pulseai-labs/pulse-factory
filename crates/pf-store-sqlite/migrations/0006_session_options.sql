-- Migration 0006: the runtime options of a chat session (ADR-0003, ADR-0007, ADR-0015). Forward-only.
--
-- Every chat has runtime controls, `artifacts` included, and a change belongs to the session: it never
-- changes the agent's defaults. The column is a JSON object of text values. A session that was live
-- before this migration reads `{}`, and the controller launches it with its agent's options, so its
-- behaviour does not change. A new session copies the agent's options in the commit that starts it.

ALTER TABLE chat_sessions ADD COLUMN runtime_options TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(runtime_options) AND json_type(runtime_options) = 'object');
