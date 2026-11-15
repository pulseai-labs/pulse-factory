// The chat state of the shell (r1.s3.w5).
//
// One `ChatProvider` sits inside the `ConnectionProvider`, so a new pairing remounts it and drops
// everything below. It holds the live sessions, the items of each session the operator opened (by
// `seq`), the delta text of the turn in progress, the running tool calls and the last `SessionList`.
//
// One stream at a time (A8, A10, and the six connections of an HTTP/1.1 page): the overview stream
// (`openStream({})`) while no chat is on screen, and `openStream({session, after})` while one is.
// The UI never polls and sets no timer here: `pf-client` reconnects, and the UI opens a new stream
// only on an operator action, when the chat on screen changes, or when the link comes back
// (`epoch`). See docs/contracts/ui-transport.md, "When the UI calls".
import type { ChatFrame, ChatItemRecord, ChatSessionRecord, RequestFrame, SessionList } from "@pulse-factory/api-client";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { TransportError, type StreamEvent, type StreamHandle, type Transport } from "../transport/types";
import { asTransportError, isDownKind, useConnection } from "./connection";

export type StreamStatus =
  | { state: "idle" }
  | { state: "open" }
  /** `pf-client` waits `retryInMs` and tries again by itself; `at` is when the event came (the banner counts down from it). */
  | { state: "reconnecting"; reason: string; retryInMs: number; at: number }
  /** The stream ended with a link failure: the 3c banner shows it. */
  | { state: "down" }
  /** The stream ended with a refusal or a server error: one error line and Reconnect. */
  | { state: "error"; message: string };

/** Why no stream is open: the link is down, the device is unpaired, or the operator must reconnect. */
type Halt = { kind: "link" } | { kind: "auth" } | { kind: "manual" };

interface State {
  /** Every session record the UI has seen, ended ones too: an open chat keeps its record. */
  sessions: Record<string, ChatSessionRecord>;
  items: Record<string, Record<number, ChatItemRecord>>;
  deltas: Record<string, string>;
  /** `tool_running` calls with no finished item yet: session, call ID, tool. */
  running: Record<string, Record<string, string>>;
  list: SessionList | null;
}

type Action =
  | { type: "frame"; frame: ChatFrame }
  | { type: "record"; record: ChatSessionRecord }
  | { type: "list"; list: SessionList };

const INITIAL: State = { sessions: {}, items: {}, deltas: {}, running: {}, list: null };

function mergeRecord(sessions: Record<string, ChatSessionRecord>, record: ChatSessionRecord) {
  const held = sessions[record.id];
  // An older record never replaces a newer one (an answer and a frame can arrive in either order).
  if (held && held.updated_at > record.updated_at) return sessions;
  return { ...sessions, [record.id]: record };
}

function without<T>(map: Record<string, T>, key: string): Record<string, T> {
  if (!(key in map)) return map;
  const next = { ...map };
  delete next[key];
  return next;
}

function reduce(state: State, action: Action): State {
  switch (action.type) {
    case "record":
      return { ...state, sessions: mergeRecord(state.sessions, action.record) };
    case "list": {
      let sessions = state.sessions;
      for (const record of action.list.sessions) sessions = mergeRecord(sessions, record);
      return { ...state, sessions, list: action.list };
    }
    case "frame": {
      const frame = action.frame;
      switch (frame.type) {
        case "hello": {
          // A hello holds every live session: one held as live and missing here has ended.
          let sessions = state.sessions;
          const ids = new Set<string>();
          for (const record of frame.sessions) {
            ids.add(record.id);
            sessions = mergeRecord(sessions, record);
          }
          for (const [id, record] of Object.entries(sessions)) {
            if (record.ended_at === null && !ids.has(id)) {
              sessions = { ...sessions, [id]: { ...record, ended_at: record.updated_at } };
            }
          }
          return { ...state, sessions };
        }
        case "session":
          return { ...state, sessions: mergeRecord(state.sessions, frame.session) };
        case "item": {
          const item = frame.item;
          const held = state.items[item.session_id] ?? {};
          const isNew = !(item.seq in held);
          const items = { ...state.items, [item.session_id]: { ...held, [item.seq]: item } };
          let { deltas, running } = state;
          const body = item.body;
          if (isNew && (body.kind === "assistant_text" || body.kind === "turn_result")) {
            deltas = without(deltas, item.session_id);
          }
          if (body.kind === "tool_call" && body.finished) {
            const calls = running[item.session_id];
            if (calls && body.call_id in calls) {
              const rest = { ...calls };
              delete rest[body.call_id];
              running = { ...running, [item.session_id]: rest };
            }
          }
          if (isNew && body.kind === "turn_result") running = without(running, item.session_id);
          return { ...state, items, deltas, running };
        }
        case "delta":
          return {
            ...state,
            deltas: { ...state.deltas, [frame.session_id]: (state.deltas[frame.session_id] ?? "") + frame.text },
          };
        case "tool_running":
          return {
            ...state,
            running: {
              ...state.running,
              [frame.session_id]: { ...(state.running[frame.session_id] ?? {}), [frame.call_id]: frame.tool },
            },
          };
        case "caught":
          return state;
      }
    }
  }
}

export interface Chat {
  records: Record<string, ChatSessionRecord>;
  /** The live sessions (no `ended_at`), in creation order. */
  live: ChatSessionRecord[];
  items: Record<string, Record<number, ChatItemRecord>>;
  deltas: Record<string, string>;
  running: Record<string, Record<string, string>>;
  list: SessionList | null;
  status: StreamStatus;
  /** The chat on screen, which the stream follows. `null`: the overview. */
  following: string | null;
  follow(session: string | null): void;
  /** The operator's "Reconnect" after a stream error. */
  reconnect(): void;
  /** A verb's answer carries the session's record. */
  applyRecord(record: ChatSessionRecord): void;
  /** One `sessionList` call. It resolves to the error text of a refusal, or `null`. */
  refreshList(): Promise<string | null>;
  /** `refreshList` once: the first time a page or a chat needs the list. */
  ensureList(): Promise<void>;
  /**
   * Listen for the request frames of the one stream (r1.s4.w3, W1). The requests state is the one
   * listener; it opens no stream. Returns the function that stops the listening.
   */
  onRequestFrame(listener: (frame: RequestFrame) => void): () => void;
}

const Ctx = createContext<Chat | null>(null);

export function useChat(): Chat {
  const c = useContext(Ctx);
  if (!c) throw new Error("useChat outside a ChatProvider");
  return c;
}

/**
 * The derived display state "not connected" (r1.s6.w1, P2): the link is down, or the open stream is
 * `reconnecting`. 11i's banner, the dimmed page, the grey pill and the "As of" subtitle all key on it. It
 * changes no state machine and adds no timer.
 */
export function useNotConnected(): boolean {
  const { link } = useConnection();
  const { status } = useChat();
  return link.state === "down" || status.state === "reconnecting";
}

export function stateLabel(state: ChatSessionRecord["state"]): string {
  return state === "working" ? "Working" : state === "needs_you" ? "Needs you" : "Idle";
}

/** The display name of an agent: the server's, else its ID. */
export function agentName(list: SessionList | null, agentId: string): string {
  const agent = list?.agents.find((a) => a.id === agentId);
  if (agent) return agent.name;
  return agentId === "orchestrator" ? "Orchestrator" : agentId;
}

function byCreation(a: ChatSessionRecord, b: ChatSessionRecord): number {
  if (a.created_at !== b.created_at) return a.created_at < b.created_at ? -1 : 1;
  return a.id.localeCompare(b.id, undefined, { numeric: true });
}

export function ChatProvider(props: { transport: Transport; children: ReactNode }) {
  const { transport, children } = props;
  const { call, report, epoch, link } = useConnection();
  const [state, dispatch] = useReducer(reduce, INITIAL);
  const [following, setFollowing] = useState<string | null>(null);
  const [halt, setHalt] = useState<Halt | null>(null);
  const [status, setStatus] = useState<StreamStatus>({ state: "idle" });
  const [openToken, setOpenToken] = useState(0);
  // The highest `seq` held for each session: what a stream opens `after`.
  const maxSeq = useRef<Record<string, number>>({});
  const listLoaded = useRef(false);
  const requestListeners = useRef(new Set<(frame: RequestFrame) => void>());

  const onEvent = useCallback(
    (event: StreamEvent) => {
      switch (event.event) {
        case "frame": {
          const frame = event.frame;
          if (frame.type === "item") {
            const held = maxSeq.current[frame.item.session_id] ?? 0;
            maxSeq.current[frame.item.session_id] = Math.max(held, frame.item.seq);
          }
          dispatch({ type: "frame", frame });
          setStatus((s) => (s.state === "open" ? s : { state: "open" }));
          return;
        }
        case "request_frame":
          // A request frame is a sign that the stream is open, as a chat frame is. It rides this one
          // stream, so Needs you opens none of its own.
          for (const listener of requestListeners.current) listener(event.frame);
          setStatus((s) => (s.state === "open" ? s : { state: "open" }));
          return;
        case "reconnecting":
          // `pf-client` waits and tries again by itself: the UI shows it and does nothing else.
          setStatus({ state: "reconnecting", reason: event.reason, retryInMs: event.retry_in_ms, at: Date.now() });
          return;
        case "closed": {
          const { kind, message } = event.error;
          if (kind === "unreachable" || kind === "rate_limited") {
            // The local leg dropped, or the server limits calls: the banner, and a new stream when
            // the link comes back.
            report(new TransportError(kind, message));
            setStatus({ state: "down" });
            setHalt({ kind: "link" });
          } else if (kind === "unauthorized" || kind === "key_changed") {
            // Only a new pairing, which remounts this provider, opens a stream again.
            report(new TransportError(kind, message));
            setStatus({ state: "down" });
            setHalt({ kind: "auth" });
          } else {
            setStatus({ state: "error", message });
            setHalt({ kind: "manual" });
          }
          return;
        }
      }
    },
    [report],
  );

  // The one open stream. It follows the chat on screen, or the overview when there is none.
  useEffect(() => {
    if (halt) return;
    let current = true;
    let handle: StreamHandle | null = null;
    const options = following ? { session: following, after: maxSeq.current[following] ?? 0 } : {};
    setStatus({ state: "idle" });
    transport
      .openStream(options, (event) => {
        if (current) onEvent(event);
      })
      .then(
        (h) => {
          if (current) handle = h;
          else void h.close();
        },
        () => {
          // `openStream` never rejects for a stream failure; nothing to do for a bug elsewhere.
        },
      );
    return () => {
      current = false;
      if (handle) void handle.close();
    };
  }, [transport, following, halt, openToken, onEvent]);

  // Moving to another chat is an operator action: it clears a stream error.
  useEffect(() => {
    setHalt((h) => (h && h.kind === "manual" ? null : h));
  }, [following]);

  const refreshList = useCallback(async (): Promise<string | null> => {
    try {
      const list = await call((t) => t.sessionList());
      dispatch({ type: "list", list });
      listLoaded.current = true;
      return null;
    } catch (e) {
      const err = asTransportError(e);
      // A disconnect shows the banner. Any other refusal is one error line for the page.
      return isDownKind(err.kind) ? null : err.message;
    }
  }, [call]);

  const ensureList = useCallback(async () => {
    if (listLoaded.current) return;
    listLoaded.current = true;
    const failure = await refreshList();
    if (failure !== null) listLoaded.current = false;
  }, [refreshList]);

  // When the link comes back: the session list once, and a new stream if a link failure ended it.
  // The link comes back either by a `status()` retry (`epoch` rises) or by any call that reaches
  // the server (the link goes from down to up without a new `epoch`).
  const lastEpoch = useRef(epoch);
  const lastLinkState = useRef(link.state);
  useEffect(() => {
    const cameBack = lastLinkState.current !== "up" && link.state === "up";
    lastLinkState.current = link.state;
    if (epoch === lastEpoch.current && !cameBack) return;
    lastEpoch.current = epoch;
    void refreshList();
    setHalt((h) => (h && h.kind === "link" ? null : h));
  }, [epoch, link.state, refreshList]);

  const follow = useCallback((session: string | null) => setFollowing(session), []);
  const reconnect = useCallback(() => {
    setHalt(null);
    setOpenToken((n) => n + 1);
  }, []);
  const applyRecord = useCallback((record: ChatSessionRecord) => dispatch({ type: "record", record }), []);
  const onRequestFrame = useCallback((listener: (frame: RequestFrame) => void) => {
    requestListeners.current.add(listener);
    return () => {
      requestListeners.current.delete(listener);
    };
  }, []);

  const live = useMemo(
    () => Object.values(state.sessions).filter((s) => s.ended_at === null).sort(byCreation),
    [state.sessions],
  );

  const value = useMemo<Chat>(
    () => ({
      records: state.sessions,
      live,
      items: state.items,
      deltas: state.deltas,
      running: state.running,
      list: state.list,
      status,
      following,
      follow,
      reconnect,
      applyRecord,
      refreshList,
      ensureList,
      onRequestFrame,
    }),
    [state, live, status, following, follow, reconnect, applyRecord, refreshList, ensureList, onRequestFrame],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
