// The transport interface (docs/contracts/ui-transport.md). The UI reaches the server only through it.
// The types come from the generated API package. No method takes or returns the device token: the
// token lives on the native side (the test bridge, or the Tauri shell's Rust side).
import type {
  ApiStatus,
  ArmActed,
  ChatFrame,
  ConfirmCodeArgs,
  ConfirmIssued,
  DeviceAckArgs,
  DeviceAnswerArgs,
  DeviceArmArgs,
  DeviceClearArgs,
  DeviceRecord,
  FolderList,
  FolderListArgs,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
  ProjectRemoveArgs,
  RequestActed,
  RequestDetail,
  RequestFrame,
  RequestList,
  RequestsArgs,
  RunList,
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
  SettingChanged,
  SettingsGetArgs,
  SettingsSetArgs,
  SettingsUnsetArgs,
  SettingsView,
  ShowArgs,
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
  /**
   * The word of a refused device verb (`code_used`, `code_expired`, …), when the server gave one. The
   * confirm dialog words its message by it, not by the server's text (r1.s4.w3, W3).
   */
  readonly refusal?: string;
  constructor(kind: TransportErrorKind, message: string, refusal?: string) {
    super(message);
    this.name = "TransportError";
    this.kind = kind;
    if (refusal !== undefined) this.refusal = refusal;
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
 * A `request_frame` carries the requests and the claim; it rides the one stream the app holds, beside
 * the chat frames and not inside them (r1.s4.w3, W1, W2).
 */
export type StreamEvent =
  | { event: "frame"; frame: ChatFrame }
  | { event: "request_frame"; frame: RequestFrame }
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * A request frame, when its fields have the shape the requests state reads: `requests` an array and
 * `claim` an object or null; `request` an object with a string `id`, a number `revision` and a string
 * `status`; `claim` an object or null. Anything else is `null` and is dropped (never a closed stream).
 */
function parseRequestFrame(value: unknown): RequestFrame | null {
  if (!isObject(value)) return null;
  const nullableObject = (v: unknown) => v === null || isObject(v);
  switch (value.type) {
    case "requests":
      return Array.isArray(value.requests) && nullableObject(value.claim) ? (value as unknown as RequestFrame) : null;
    case "request": {
      const r = value.request;
      return isObject(r) && typeof r.id === "string" && typeof r.revision === "number" && typeof r.status === "string"
        ? (value as unknown as RequestFrame)
        : null;
    }
    case "claim":
      return "claim" in value && nullableObject(value.claim) ? (value as unknown as RequestFrame) : null;
    default:
      return null;
  }
}

/**
 * A native value as a `StreamEvent`, or `null` when it is none of the four shapes. Both
 * implementations use this, so a value of another shape is dropped the same way in each. A chat frame
 * is checked by its `type` only: the generated `ChatFrame` is the native side's to decode. A request
 * frame is checked by its type and the fields the requests state reads.
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
  if (v.event === "request_frame") {
    const frame = parseRequestFrame(v.frame);
    return frame === null ? null : { event: "request_frame", frame };
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
  /** Ends the project's live sessions with `end_sessions`, then removes it. A run that is active refuses it (r1.s5.w5). */
  projectRemove(args: ProjectRemoveArgs): Promise<ProjectChanged>;
  /** The folders of one path inside the server's home folder; no path is the home folder (r1.s5.w5). */
  folderList(args?: FolderListArgs): Promise<FolderList>;
  sessionList(args?: SessionListArgs): Promise<SessionList>;
  /** A state-changing call carries a `command_id`. A retry of the same submit reuses it (A7). */
  sessionStart(args: SessionStartArgs): Promise<SessionStarted>;
  sessionSend(args: SessionSendArgs): Promise<SessionChanged>;
  sessionAnswer(args: SessionAnswerArgs): Promise<SessionChanged>;
  sessionInterrupt(args: SessionInterruptArgs): Promise<SessionChanged>;
  sessionSet(args: SessionSetArgs): Promise<SessionChanged>;
  sessionEnd(args: SessionEndArgs): Promise<SessionEnded>;
  /** The open requests (all of them with `all`) and the claim (r1.s4.w3). */
  requests(args?: RequestsArgs): Promise<RequestList>;
  show(args: ShowArgs): Promise<RequestDetail>;
  /** One-use code for one act. It binds the subject; the dialog shows what it binds. */
  confirmCode(args: ConfirmCodeArgs): Promise<ConfirmIssued>;
  answer(args: DeviceAnswerArgs): Promise<RequestActed>;
  clear(args: DeviceClearArgs): Promise<RequestActed>;
  ack(args: DeviceAckArgs): Promise<RequestActed>;
  settingsGet(args?: SettingsGetArgs): Promise<SettingsView>;
  settingsSet(args: SettingsSetArgs): Promise<SettingChanged>;
  settingsUnset(args: SettingsUnsetArgs): Promise<SettingChanged>;
  /** Every run the controller holds: its state, its waiting text, its open request count. */
  runList(): Promise<RunList>;
  /** Arms the run of a start card with a code from `confirmCode` (subject kind `start`). */
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
