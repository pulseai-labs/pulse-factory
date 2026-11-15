// The selected agent of the Agents page (r1.s5.w4, Approach 5): a module-level store that `AgentsSidebar` (the
// list) and `Agents.tsx` (the detail) both read with `useSyncExternalStore`. It makes no call and keeps nothing
// in storage. It starts on the first agent the server lists (the Orchestrator); a selected ID the server no
// longer lists falls back to the first.
import type { AgentRecord } from "@pulse-factory/api-client";
import { useSyncExternalStore } from "react";

let selected: string | null = null;
const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function emit(next: string | null): void {
  if (next === selected) return;
  selected = next;
  for (const listener of listeners) listener();
}

/** A click on a row of the agent list. */
export function selectAgent(id: string): void {
  emit(id);
}

/** Forget the selection (the tests call it between cases). */
export function resetAgentSelection(): void {
  emit(null);
}

/** The selected agent's ID, or `null` while none was chosen. */
export function useSelectedAgentId(): string | null {
  return useSyncExternalStore(subscribe, () => selected, () => null);
}

/** The agent the page shows: the selected one if the server lists it, else the first. */
export function useSelectedAgent(agents: AgentRecord[]): AgentRecord | undefined {
  const id = useSelectedAgentId();
  return agents.find((a) => a.id === id) ?? agents[0];
}

/** The Orchestrator: the one agent whose live sessions the server limits to one for each project (`ORCHESTRATOR_AGENT`, `controller/sessions.rs`). */
export function isOrchestrator(agent: AgentRecord): boolean {
  return agent.id === "orchestrator";
}
