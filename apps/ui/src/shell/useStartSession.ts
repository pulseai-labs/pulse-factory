// "New session" of a project (Approach 8), for the sidebar's row and the Projects page's block. When the
// project has a live session of the Orchestrator, it opens that chat and calls nothing. When it has none,
// it calls `sessionStart` with the project, the Orchestrator's agent ID and a command ID that a retry after
// `unreachable` reuses (the rule of `Agents.tsx`), reads the session list again, and opens the new chat.
import { useEffect, useRef, useState } from "react";
import { useCommandId } from "../components/command-id";
import { asTransportError, useConnection } from "../state/connection";
import { useChat } from "../state/chat";
import { useNavigation } from "./navigation";

const ORCHESTRATOR = "orchestrator";

export interface StartSession {
  /** Opens the project's Orchestrator chat, or starts one. */
  start(projectId: string): Promise<void>;
  /** The project whose start is in flight, or `null`. */
  busy: string | null;
  /** The one error line of a project's last start, or `null`. */
  errorOf(projectId: string): string | null;
}

export function useStartSession(): StartSession {
  const { call } = useConnection();
  const chat = useChat();
  const { openSession } = useNavigation();
  const command = useCommandId();
  const [busy, setBusy] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  async function start(projectId: string): Promise<void> {
    if (busy !== null) return;
    setErrors((e) => {
      if (!(projectId in e)) return e;
      const rest = { ...e };
      delete rest[projectId];
      return rest;
    });
    const held = chat.live.find((s) => s.project_id === projectId && s.agent_id === ORCHESTRATOR);
    if (held) {
      openSession(held.id);
      return;
    }
    setBusy(projectId);
    // A retry of the same start after `unreachable` reuses the ID, so the server starts one session.
    const id = command.idFor(`start:${projectId}:${ORCHESTRATOR}`);
    try {
      const started = await call((t) => t.sessionStart({ project_id: projectId, agent_id: ORCHESTRATOR, command_id: id }));
      command.clear();
      // The record of the new session comes with the list.
      await chat.refreshList();
      openSession(started.session_id);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") command.clear();
      if (live.current) setErrors((all) => ({ ...all, [projectId]: err.message }));
    } finally {
      if (live.current) setBusy(null);
    }
  }

  return { start, busy, errorOf: (projectId) => errors[projectId] ?? null };
}
