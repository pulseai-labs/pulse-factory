// The requests state of the shell (r1.s4.w3): the requests the server holds, the current claim, the
// bodies the Needs you page read and the last run list.
//
// A `RequestsProvider` sits inside the `ChatProvider` and gets its frames from the one stream that
// provider holds (W1): the controller sends request frames on every stream, the overview included,
// so Needs you needs no stream of its own and the rail's count is fed with no call. The page reads
// `requests` when it opens (G3, the correctness path); nothing here calls the transport and nothing
// here sets a timer.
//
// The rules are a pure reducer (`reduceRequests`), tested on fixed sequences. A frame can arrive
// twice across the snapshot, so a request is kept by ID and judged by its revision and status.
import type {
  ClaimRecord,
  RequestActed,
  RequestDetail,
  RequestFrame,
  RequestList,
  RequestSummary,
  RunList,
} from "@pulse-factory/api-client";
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, type ReactNode } from "react";
import { useChat } from "./chat";

export interface RequestsState {
  /** Every request the UI has seen, by ID: the open ones, and the closed ones whose closing is known. */
  byId: Record<string, RequestSummary>;
  claim: ClaimRecord | null;
  /** The bodies `show` answered, by `<id>@<revision>`. */
  bodies: Record<string, RequestDetail>;
  runs: RunList | null;
  /** A `requests` snapshot or read has arrived. */
  loaded: boolean;
}

export type RequestsAction =
  | { type: "frame"; frame: RequestFrame }
  /** The answer of a `requests` read. */
  | { type: "read"; list: RequestList }
  /** The request an act answered (`RequestActed.request`): it applies like a frame. */
  | { type: "acted"; acted: RequestActed }
  | { type: "body"; detail: RequestDetail }
  | { type: "runs"; runs: RunList };

export const INITIAL_REQUESTS: RequestsState = { byId: {}, claim: null, bodies: {}, runs: null, loaded: false };

export function bodyKey(id: string, revision: number): string {
  return `${id}@${revision}`;
}

/**
 * One request against the one held. A lower revision is ignored. At the same revision a status other
 * than `open` wins over `open` (a request leaves `open` once), and a duplicate changes nothing.
 */
function applyRequest(byId: Record<string, RequestSummary>, incoming: RequestSummary): Record<string, RequestSummary> {
  const held = byId[incoming.id];
  if (held) {
    if (incoming.revision < held.revision) return byId;
    if (incoming.revision === held.revision) {
      // Only `open` -> not `open` moves a request at one revision.
      if (!(held.status === "open" && incoming.status !== "open")) return byId;
    }
  }
  return { ...byId, [incoming.id]: incoming };
}

/**
 * A claim against the one held. A lower generation is ignored; the same generation applies, so a
 * revoke of the holder shows. `null` is the server's answer that no claim exists.
 */
function applyClaim(held: ClaimRecord | null, incoming: ClaimRecord | null): ClaimRecord | null {
  if (incoming === null) return held === null ? held : null;
  if (held !== null && incoming.generation < held.generation) return held;
  if (held !== null && JSON.stringify(held) === JSON.stringify(incoming)) return held;
  return incoming;
}

/**
 * A snapshot (a `requests` frame, a `requests` read): the open requests it lists replace the set of
 * open requests. A held open request that the snapshot does not list at all is no longer open; it is
 * dropped, as its closing status is not known. A closed request that the snapshot lists applies by
 * the rules above.
 */
function applySnapshot(
  state: RequestsState,
  requests: RequestSummary[],
  claim: ClaimRecord | null,
): RequestsState {
  const listed = new Set(requests.map((r) => r.id));
  let byId = state.byId;
  for (const held of Object.values(byId)) {
    if (held.status === "open" && !listed.has(held.id)) {
      const rest = { ...byId };
      delete rest[held.id];
      byId = rest;
    }
  }
  for (const request of requests) byId = applyRequest(byId, request);
  const nextClaim = applyClaim(state.claim, claim);
  if (byId === state.byId && nextClaim === state.claim && state.loaded) return state;
  return { ...state, byId, claim: nextClaim, loaded: true };
}

export function reduceRequests(state: RequestsState, action: RequestsAction): RequestsState {
  switch (action.type) {
    case "frame": {
      const frame = action.frame;
      switch (frame.type) {
        case "requests":
          return applySnapshot(state, frame.requests, frame.claim);
        case "request": {
          const byId = applyRequest(state.byId, frame.request);
          return byId === state.byId ? state : { ...state, byId };
        }
        case "claim": {
          const claim = applyClaim(state.claim, frame.claim);
          return claim === state.claim ? state : { ...state, claim };
        }
      }
      return state;
    }
    case "read":
      return applySnapshot(state, action.list.requests, action.list.claim);
    case "acted": {
      const { acted } = action;
      let next = state;
      if (acted.request) {
        const byId = applyRequest(next.byId, acted.request);
        if (byId !== next.byId) next = { ...next, byId };
      }
      // An act moves the claim to this device (ADR-0009). The claim frame that follows replaces this.
      if (!acted.replayed && acted.generation !== null) {
        const held = next.claim;
        if (held === null || acted.generation >= held.generation) {
          const claim = applyClaim(held, {
            generation: acted.generation,
            holder: `device ${acted.device}`,
            device: acted.device,
            revoked: false,
            claimed_at: acted.request?.answered_at ?? held?.claimed_at ?? "",
          });
          if (claim !== held) next = { ...next, claim };
        }
      }
      return next;
    }
    case "body": {
      const key = bodyKey(action.detail.id, action.detail.revision);
      return { ...state, bodies: { ...state.bodies, [key]: action.detail } };
    }
    case "runs":
      return { ...state, runs: action.runs };
  }
}

/** The open requests: `merge_ack` first (the spine merge approval on top), then by when they opened. */
export function openRequests(state: RequestsState): RequestSummary[] {
  return Object.values(state.byId)
    .filter((r) => r.status === "open")
    .sort((a, b) => {
      const ak = a.kind === "merge_ack" ? 0 : 1;
      const bk = b.kind === "merge_ack" ? 0 : 1;
      if (ak !== bk) return ak - bk;
      if (a.opened_at !== b.opened_at) return a.opened_at < b.opened_at ? -1 : 1;
      return a.id.localeCompare(b.id, undefined, { numeric: true });
    });
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** The requests answered or withdrawn in the last 24 hours before `now`, newest first. */
export function answeredRecently(state: RequestsState, now: number): RequestSummary[] {
  return Object.values(state.byId)
    .filter((r) => {
      if (r.status !== "answered" && r.status !== "withdrawn") return false;
      if (r.answered_at === null) return false;
      const at = Date.parse(r.answered_at);
      return !Number.isNaN(at) && now - at <= DAY_MS && at <= now + DAY_MS;
    })
    .sort((a, b) => {
      const at = a.answered_at ?? "";
      const bt = b.answered_at ?? "";
      if (at !== bt) return at < bt ? 1 : -1;
      return b.id.localeCompare(a.id, undefined, { numeric: true });
    });
}

export interface Requests {
  state: RequestsState;
  /** The open requests, in the order the page lists them. The rail's badge counts these. */
  open: RequestSummary[];
  apply(action: RequestsAction): void;
}

const Ctx = createContext<Requests | null>(null);

export function useRequests(): Requests {
  const c = useContext(Ctx);
  if (!c) throw new Error("useRequests outside a RequestsProvider");
  return c;
}

export function RequestsProvider(props: { children: ReactNode }) {
  const chat = useChat();
  const [state, dispatch] = useReducer(reduceRequests, INITIAL_REQUESTS);
  const apply = useCallback((action: RequestsAction) => dispatch(action), []);

  // The one source of frames is the chat provider's stream. A child's effect runs before its
  // parent's, so this listens before the stream opens.
  const { onRequestFrame } = chat;
  useEffect(() => onRequestFrame((frame) => dispatch({ type: "frame", frame })), [onRequestFrame]);

  const open = useMemo(() => openRequests(state), [state]);
  const value = useMemo<Requests>(() => ({ state, open, apply }), [state, open, apply]);
  return <Ctx.Provider value={value}>{props.children}</Ctx.Provider>;
}
