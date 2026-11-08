// The transport inside the Mac app (docs/contracts/ui-transport.md): the six operations are the six
// Tauri commands of the shell's Rust side, with the names of the interface in snake case. The Rust
// side holds the device token in the Keychain and makes the pinned TLS call. This file sees no token
// and opens no connection: its only channel is `invoke`.
import type { ApiStatus, ProjectAddArgs, ProjectChanged, ProjectList } from "@pulse-factory/api-client";
import { invoke } from "@tauri-apps/api/core";
import {
  TRANSPORT_ERROR_KINDS,
  TransportError,
  type PairedServer,
  type Transport,
  type TransportErrorKind,
} from "./types";

type InvokeLike = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

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

export function createTauriTransport(invokeImpl?: InvokeLike): Transport {
  const call: InvokeLike = invokeImpl ?? ((command, args) => invoke(command, args));

  async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
    try {
      return (await call(name, args)) as T;
    } catch (rejection) {
      throw toTransportError(rejection);
    }
  }

  return {
    paired: () => command<PairedServer | null>("paired", undefined),
    pair: (code, deviceName) => command<PairedServer>("pair", { code, deviceName }),
    status: () => command<ApiStatus>("status", undefined),
    projectList: () => command<ProjectList>("project_list", undefined),
    projectAdd: (args: ProjectAddArgs) => command<ProjectChanged>("project_add", { args }),
    forget: async () => {
      await command<null>("forget", undefined);
    },
  };
}
