// A command ID for each state-changing verb (A7). One operator action gets one ID. A retry of the
// same action after `unreachable` reuses it, so the server admits the action once.
import { useRef } from "react";

export function newCommandId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * The command ID of the action that is in flight or that ended with `unreachable`. `idFor(key)`
 * returns the held ID when the key is the same action, and a fresh one otherwise. `clear()` ends the
 * ID: after an answer, after a refusal and after an answer with a `note`.
 */
export function useCommandId() {
  const held = useRef<{ key: string; id: string } | null>(null);
  return {
    idFor(key: string): string {
      if (held.current && held.current.key === key) return held.current.id;
      const id = newCommandId();
      held.current = { key, id };
      return id;
    },
    clear(): void {
      held.current = null;
    },
  };
}
