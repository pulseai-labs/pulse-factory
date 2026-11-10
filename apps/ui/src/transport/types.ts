// The transport interface (docs/contracts/ui-transport.md). The UI reaches the server only through it.
// The types come from the generated API package. No method takes or returns the device token: the
// token lives on the native side (the test bridge, or the Tauri shell's Rust side).
import type {
  ApiStatus,
  ArmActed,
  ChatFrame,
  ConfirmCodeArgs,
  ConfirmIssued,
  DeviceArmArgs,
  DeviceRecord,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
  ServerRef,
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

export type TransportErrorKind =
  | "unreachable" // no HTTP answer: refused, reset, timed out, name not found
  | "unauthorized" // 401 on a token route: a revoked, idle or unknown device
  | "code_refused" // 401 on /v1/pair: a used, expired, replaced or malformed code
  | "key_changed" // the server key is not the pinned one; nothing was sent
  | "forbidden" // 403
  | "refused" // 422: the handler's refusal, its text kept
  | "rate_limited" // 429
  | "bad_request" // 400 or 413
  | "server"; // 5xx, or an answer that is not the documented JSON

export const TRANSPORT_ERROR_KINDS: readonly TransportErrorKind[] = [
  "unreachable",
  "unauthorized",
  "code_refused",
  "key_changed",
  "forbidden",
  "refused",
  "rate_limited",
  "bad_request",
  "server",
];

/** The message of a `key_changed` error, word for word (the client rule in docs/contracts/api.md). */
export const KEY_CHANGED_MESSAGE = "server key changed: re-pair";

export class TransportError extends Error {
  readonly kind: TransportErrorKind;
  constructor(kind: TransportErrorKind, message: string) {
    super(message);
    this.name = "TransportError";
    this.kind = kind;
  }
}

export interface PairedServer {
  address: string;
  device: DeviceRecord;
  /** The server the last answer named. Known only after an answer, so it can be absent. */
  server?: ServerRef;
}

/** What `sessionStart` returns: the transport's own shape (no wire type), from the controller's answer. */
export interface SessionStarted {
  replayed: boolean;
  session_id: string;
  worktree_id: string;
  worktree_dir: string;
  note: string | null;
}

/** What `sessionEnd` returns. */
export interface SessionEnded {
  replayed: boolean;
  session_id: string;
  note: string | null;
}

/** What to follow: one session and its items after `after` (the last `seq` the UI holds; 0 if omitted). */
export interface StreamOptions {
  session?: string | null;
  after?: number;
}

/**
 * One thing a chat stream tells the UI. `closed` is the last event: no event follows it or a
 * `close()`. The reconnect is native: while `reconnecting` the native side waits and tries again.
 */
export type StreamEvent =
  | { event: "frame"; frame: ChatFrame }
  | { event: "reconnecting"; after: number; retry_in_ms: number; reason: string }
  | { event: "closed"; error: { kind: TransportErrorKind; message: string } };

export interface StreamHandle {
  /** Ends the stream. No event follows. */
  close(): Promise<void>;
}

const FRAME_TYPES = ["hello", "session", "item", "caught", "delta", "tool_running"] as const;

function isKind(value: unknown): value is TransportErrorKind {
  return typeof value === "string" && (TRANSPORT_ERROR_KINDS as readonly string[]).includes(value);
}

/**
 * A native value as a `StreamEvent`, or `null` when it is none of the three shapes. Both
 * implementations use this, so a value of another shape is dropped the same way in each. A frame
 * is checked by its `type` only: the generated `ChatFrame` is the native side's to decode.
 */
export function parseStreamEvent(value: unknown): StreamEvent | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  if (v.event === "frame") {
    const frame = v.frame as { type?: unknown } | null | undefined;
    if (typeof frame !== "object" || frame === null) return null;
    if (!(FRAME_TYPES as readonly unknown[]).includes(frame.type)) return null;
    return { event: "frame", frame: frame as ChatFrame };
  }
  if (v.event === "reconnecting") {
    if (typeof v.after !== "number" || typeof v.retry_in_ms !== "number" || typeof v.reason !== "string") return null;
    return { event: "reconnecting", after: v.after, retry_in_ms: v.retry_in_ms, reason: v.reason };
  }
  if (v.event === "closed") {
    const error = v.error as { kind?: unknown; message?: unknown } | null | undefined;
    if (typeof error !== "object" || error === null) return null;
    if (!isKind(error.kind) || typeof error.message !== "string") return null;
    return { event: "closed", error: { kind: error.kind, message: error.message } };
  }
  return null;
}

export interface Transport {
  /** No network call. `null` when no server is paired. */
  paired(): Promise<PairedServer | null>;
  pair(code: string, deviceName: string): Promise<PairedServer>;
  status(): Promise<ApiStatus>;
  projectList(): Promise<ProjectList>;
  projectAdd(args: ProjectAddArgs): Promise<ProjectChanged>;
  sessionList(args?: SessionListArgs): Promise<SessionList>;
  /** A state-changing call carries a `command_id`. A retry of the same submit reuses it (A7). */
  sessionStart(args: SessionStartArgs): Promise<SessionStarted>;
  sessionSend(args: SessionSendArgs): Promise<SessionChanged>;
  sessionAnswer(args: SessionAnswerArgs): Promise<SessionChanged>;
  sessionInterrupt(args: SessionInterruptArgs): Promise<SessionChanged>;
  sessionSet(args: SessionSetArgs): Promise<SessionChanged>;
  sessionEnd(args: SessionEndArgs): Promise<SessionEnded>;
  /** Asks for a one-use confirm code for a subject (a start card's arm, here). The code is shown once. */
  confirmCode(args: ConfirmCodeArgs): Promise<ConfirmIssued>;
  /** Arms the run of a start card with a code from `confirmCode`. */
  arm(args: DeviceArmArgs): Promise<ArmActed>;
  /**
   * Opens the chat stream. It never rejects for a stream failure: every failure arrives as one
   * `closed` event. The native side pushes and reconnects (with the last `seq` it holds), so the
   * UI sets no timer and makes no call to keep a stream alive.
   */
  openStream(options: StreamOptions, onEvent: (event: StreamEvent) => void): Promise<StreamHandle>;
  /** Drops the token on this device only. The server's record of the device stays until a revoke. */
  forget(): Promise<void>;
}
