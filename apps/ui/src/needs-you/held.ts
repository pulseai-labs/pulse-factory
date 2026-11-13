// Hold (r1.s5.w4, operator ruling R5): a client-only close. Hold makes no call. It sets a merge card aside on
// this Mac: the card leaves the Open list and one quiet row stays at its end. The request stays open on the
// server, and the rail's count still counts it. A held card shows again by itself when its request's
// `revision` changes (a new head SHA), and when the app reloads: the held set lives in this module's memory
// only, never in storage (d23 checks storage).
import { useSyncExternalStore } from "react";

/** The request IDs held on this Mac, each with the revision that was held. */
let held: ReadonlyMap<string, number> = new Map();
const listeners = new Set<() => void>();

function emit(next: ReadonlyMap<string, number>): void {
  held = next;
  for (const listener of listeners) listener();
}

/** Set a request aside at its current revision. */
export function hold(id: string, revision: number): void {
  emit(new Map(held).set(id, revision));
}

/** Put a held request back. */
export function show(id: string): void {
  if (!held.has(id)) return;
  const next = new Map(held);
  next.delete(id);
  emit(next);
}

/** Held means held at this revision: a new revision shows the card again. */
export function isHeld(map: ReadonlyMap<string, number>, id: string, revision: number): boolean {
  return map.get(id) === revision;
}

/** Forget every hold (a reload does this by itself; the tests call it between cases). */
export function resetHeld(): void {
  emit(new Map());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The held set, as it is now. A change re-renders the caller. */
export function useHeld(): ReadonlyMap<string, number> {
  return useSyncExternalStore(subscribe, () => held, () => held);
}
