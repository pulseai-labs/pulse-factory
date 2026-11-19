-- Migration 0010: the position of a chat session's output in its host (ADR-0003's amendment of
-- 2026-11-16, ADR-0007; `r2.s2.w5`). Forward-only.
--
-- One nullable column on `chat_sessions`. It holds one JSON object:
--   {"unit", "incarnation", "committed", "acked", "checkpoint"}
-- the host the session's output came from, the cursor after the last output line the store
-- consumed, the cursor the controller last acknowledged to the host, and the adapter's opaque state
-- at `acked`. The commit that stores an item written from host output also sets `committed`, so a
-- restart never reads an item twice and never loses one (G4). A payload field would not do: a
-- redaction replaces the payload, and a re-read would then store the redacted text again.
--
-- A session with no host output (a fake adapter, or a chat from before this migration) holds
-- NULL. No trigger changes: `chat_sessions_owner_never_changes` watches `project_id`,
-- `worktree_id` and `agent_id` only, and the redaction trigger of `chat_items` is as it was.

ALTER TABLE chat_sessions ADD COLUMN host_cursor TEXT
    CHECK (host_cursor IS NULL OR json_valid(host_cursor));
