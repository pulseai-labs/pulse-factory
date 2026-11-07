// The transport interface (docs/contracts/ui-transport.md). The UI reaches the server only through it.
// The types come from the generated API package. No method takes or returns the device token: the
// token lives on the native side (the test bridge here, the Tauri shell's Rust side in w4).
import type {
  ApiStatus,
  DeviceRecord,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
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
}

export interface Transport {
  /** No network call. `null` when no server is paired. */
  paired(): Promise<PairedServer | null>;
  pair(code: string, deviceName: string): Promise<PairedServer>;
  status(): Promise<ApiStatus>;
  projectList(): Promise<ProjectList>;
  projectAdd(args: ProjectAddArgs): Promise<ProjectChanged>;
  /** Drops the token on this device only. The server's record of the device stays until a revoke. */
  forget(): Promise<void>;
}
