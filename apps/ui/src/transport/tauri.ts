// The transport inside the Mac app (docs/contracts/ui-transport.md): each operation is a Tauri command
// of the shell's Rust side, with the names of the interface in snake case. The Rust side holds the
// device token in the Keychain and makes the pinned TLS call. This file sees no token and opens no
// connection: its channels are `invoke` and a `Channel` for the chat stream. It uses no Tauri event
// (`listen` needs a capability, and the app declares no capability file).
import type {
  ApiStatus,
  ArmActed,
  ConfirmCodeArgs,
  ConfirmIssued,
  DeviceArmArgs,
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
import { Channel, invoke } from "@tauri-apps/api/core";
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

type InvokeLike = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

/** The part of Tauri's `Channel` that the transport uses. A test injects its own. */
export interface ChannelLike {
  onmessage: (message: unknown) => void;
}
type ChannelFactory = () => ChannelLike;

/** Every command this file invokes: the commands of `main.rs` and of the contract. */
export const TAURI_COMMANDS: readonly string[] = [
  "paired",
  "pair",
  "status",
  "project_list",
  "project_add",
  "session_list",
  "session_start",
  "session_send",
  "session_answer",
  "session_interrupt",
  "session_set",
  "session_end",
  "open_stream",
  "close_stream",
  "forget",
];

function isKind(value: unknown): value is TransportErrorKind {
  return typeof value === "string" && (TRANSPORT_ERROR_KINDS as readonly string[]).includes(value);
}

/** A rejection of a command is `{kind, message}`. Anything else is a `server` error. */
function toTransportError(rejection: unknown): TransportError {
  if (typeof rejection === "object" && rejection !== null) {
    const r = rejection as { kind?: unknown; message?: unknown };
    const message = typeof r.message === "string" && r.message !== "" ? r.message : "the app refused the call";
    return new TransportError(isKind(r.kind) ? r.kind : "server", message);
  }
  return new TransportError("server", "the app refused the call");
}

export function createTauriTransport(invokeImpl?: InvokeLike, channelFactory?: ChannelFactory): Transport {
  const call: InvokeLike = invokeImpl ?? ((command, args) => invoke(command, args));
  const makeChannel: ChannelFactory = channelFactory ?? (() => new Channel<unknown>());

  async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
    try {
      return (await call(name, args)) as T;
    } catch (rejection) {
      throw toTransportError(rejection);
    }
  }

  /** The chat stream: a `Channel` that the Rust side sends each event on, and `close_stream`. */
  async function openStream(options: StreamOptions, onEvent: (event: StreamEvent) => void): Promise<StreamHandle> {
    let finished = false;
    const channel = makeChannel();
    channel.onmessage = (message) => {
      if (finished) return;
      const event = parseStreamEvent(message);
      if (event === null) return;
      if (event.event === "closed") finished = true;
      onEvent(event);
    };
    let id: number;
    try {
      id = await command<number>("open_stream", { options, onEvent: channel });
    } catch (rejection) {
      // A stream that cannot open is one `closed` event, as every other failure of a stream is.
      const error = toTransportError(rejection);
      if (!finished) {
        finished = true;
        onEvent({ event: "closed", error: { kind: error.kind, message: error.message } });
      }
      return { close: async () => {} };
    }
    return {
      close: async () => {
        const ended = finished;
        finished = true;
        if (ended) return;
        try {
          await command<boolean>("close_stream", { id });
        } catch {
          // The stream is closed for the UI either way.
        }
      },
    };
  }

  return {
    paired: () => command<PairedServer | null>("paired", undefined),
    pair: (code, deviceName) => command<PairedServer>("pair", { code, deviceName }),
    status: () => command<ApiStatus>("status", undefined),
    projectList: () => command<ProjectList>("project_list", undefined),
    projectAdd: (args: ProjectAddArgs) => command<ProjectChanged>("project_add", { args }),
    sessionList: (args?: SessionListArgs) => command<SessionList>("session_list", { args: args ?? {} }),
    sessionStart: (args: SessionStartArgs) => command<SessionStarted>("session_start", { args }),
    sessionSend: (args: SessionSendArgs) => command<SessionChanged>("session_send", { args }),
    sessionAnswer: (args: SessionAnswerArgs) => command<SessionChanged>("session_answer", { args }),
    sessionInterrupt: (args: SessionInterruptArgs) => command<SessionChanged>("session_interrupt", { args }),
    sessionSet: (args: SessionSetArgs) => command<SessionChanged>("session_set", { args }),
    sessionEnd: (args: SessionEndArgs) => command<SessionEnded>("session_end", { args }),
    // Through the generic `verb` command (O2): `TAURI_COMMANDS` gains no name here.
    confirmCode: (args: ConfirmCodeArgs) => command<ConfirmIssued>("verb", { name: "confirm_code", args }),
    arm: (args: DeviceArmArgs) => command<ArmActed>("verb", { name: "arm", args }),
    openStream,
    forget: async () => {
      await command<null>("forget", undefined);
    },
  };
}
