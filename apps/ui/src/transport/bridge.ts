// The transport over the bridge protocol (docs/contracts/ui-transport.md): HTTP on 127.0.0.1, the
// bundle's own origin. The bridge holds the device token, checks the server key and makes the TLS
// call. This file sees no token and opens no TLS connection.
import type {
  ApiStatus,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
  SessionAnswerArgs,
  SessionChanged,
  SessionEndArgs,
  SessionInterruptArgs,
  SessionList,
  SessionListArgs,
  SessionSendArgs,
  SessionSetArgs,
  SessionStartArgs,
} from "@pulse-factory/api-client";
import {
  TRANSPORT_ERROR_KINDS,
  TransportError,
  parseStreamEvent,
  type PairedServer,
  type SessionEnded,
  type SessionStarted,
  type StreamEvent,
  type StreamHandle,
  type StreamOptions,
  type Transport,
  type TransportErrorKind,
} from "./types";

type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/** The part of `EventSource` that the transport uses. A test injects its own. */
export interface EventSourceLike {
  onmessage: ((event: { data: unknown }) => void) | null;
  onerror: ((event: unknown) => void) | null;
  close(): void;
}
type EventSourceFactory = (url: string) => EventSourceLike;

function isKind(value: unknown): value is TransportErrorKind {
  return typeof value === "string" && (TRANSPORT_ERROR_KINDS as readonly string[]).includes(value);
}

export function createBridgeTransport(fetchImpl?: FetchLike, eventSourceFactory?: EventSourceFactory): Transport {
  const doFetch: FetchLike = fetchImpl ?? ((input, init) => fetch(input, init));
  const makeEventSource: EventSourceFactory = eventSourceFactory ?? ((url) => new EventSource(url) as unknown as EventSourceLike);

  async function request<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
    let res: Response;
    try {
      res = await doFetch(path, {
        method,
        cache: "no-store",
        headers: method === "POST" ? { "content-type": "application/json" } : undefined,
        body: method === "POST" ? JSON.stringify(body ?? {}) : undefined,
      });
    } catch {
      throw new TransportError("unreachable", "cannot reach the bridge");
    }
    let answer: unknown;
    try {
      answer = await res.json();
    } catch {
      throw new TransportError("server", "the bridge answered with something that is not JSON");
    }
    if (typeof answer !== "object" || answer === null || typeof (answer as { ok?: unknown }).ok !== "boolean") {
      throw new TransportError("server", "the bridge answered with something that is not its protocol");
    }
    const a = answer as { ok: boolean; result?: unknown; error?: { kind?: unknown; message?: unknown } };
    if (a.ok) return a.result as T;
    const kind = isKind(a.error?.kind) ? a.error.kind : "server";
    const message = typeof a.error?.message === "string" ? a.error.message : "the bridge refused the call";
    throw new TransportError(kind, message);
  }

  const verb = <T>(name: string, args: unknown) => request<T>("POST", `/bridge/verbs/${name}`, args);

  /** `GET /bridge/stream` as Server-Sent Events. The reconnect is the bridge's, so none here. */
  async function openStream(options: StreamOptions, onEvent: (event: StreamEvent) => void): Promise<StreamHandle> {
    const query = new URLSearchParams();
    if (options.session !== undefined && options.session !== null) query.set("session", options.session);
    if (options.after !== undefined) query.set("after", String(options.after));
    const text = query.toString();
    const url = text === "" ? "/bridge/stream" : `/bridge/stream?${text}`;
    let finished = false;
    let source: EventSourceLike | null = null;
    const finish = () => {
      finished = true;
      source?.close();
    };
    const fail = () => {
      if (finished) return;
      finish();
      onEvent({ event: "closed", error: { kind: "unreachable", message: "the bridge closed the stream" } });
    };
    try {
      source = makeEventSource(url);
    } catch {
      fail();
      return { close: async () => {} };
    }
    source.onmessage = (message) => {
      if (finished) return;
      let native: unknown;
      try {
        native = typeof message.data === "string" ? (JSON.parse(message.data) as unknown) : null;
      } catch {
        return;
      }
      const event = parseStreamEvent(native);
      if (event === null) return;
      // After `closed` the page must not reconnect by itself: close the source at once.
      if (event.event === "closed") finish();
      onEvent(event);
    };
    source.onerror = fail;
    return {
      close: async () => {
        finish();
      },
    };
  }

  return {
    paired: () => request<PairedServer | null>("GET", "/bridge/paired"),
    pair: (code, deviceName) =>
      request<PairedServer>("POST", "/bridge/pair", { code, device_name: deviceName }),
    status: () => request<ApiStatus>("POST", "/bridge/status", {}),
    projectList: () => request<ProjectList>("POST", "/bridge/verbs/project_list", {}),
    projectAdd: (args: ProjectAddArgs) => request<ProjectChanged>("POST", "/bridge/verbs/project_add", args),
    sessionList: (args?: SessionListArgs) => verb<SessionList>("session_list", args ?? {}),
    sessionStart: (args: SessionStartArgs) => verb<SessionStarted>("session_start", args),
    sessionSend: (args: SessionSendArgs) => verb<SessionChanged>("session_send", args),
    sessionAnswer: (args: SessionAnswerArgs) => verb<SessionChanged>("session_answer", args),
    sessionInterrupt: (args: SessionInterruptArgs) => verb<SessionChanged>("session_interrupt", args),
    sessionSet: (args: SessionSetArgs) => verb<SessionChanged>("session_set", args),
    sessionEnd: (args: SessionEndArgs) => verb<SessionEnded>("session_end", args),
    openStream,
    forget: async () => {
      await request<null>("POST", "/bridge/forget", {});
    },
  };
}
