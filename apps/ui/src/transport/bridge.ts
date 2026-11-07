// The transport over the bridge protocol (docs/contracts/ui-transport.md): HTTP on 127.0.0.1, the
// bundle's own origin. The bridge holds the device token, checks the server key and makes the TLS
// call. This file sees no token and opens no TLS connection.
import type { ApiStatus, ProjectAddArgs, ProjectChanged, ProjectList } from "@pulse-factory/api-client";
import {
  TRANSPORT_ERROR_KINDS,
  TransportError,
  type PairedServer,
  type Transport,
  type TransportErrorKind,
} from "./types";

type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

function isKind(value: unknown): value is TransportErrorKind {
  return typeof value === "string" && (TRANSPORT_ERROR_KINDS as readonly string[]).includes(value);
}

export function createBridgeTransport(fetchImpl?: FetchLike): Transport {
  const doFetch: FetchLike = fetchImpl ?? ((input, init) => fetch(input, init));

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

  return {
    paired: () => request<PairedServer | null>("GET", "/bridge/paired"),
    pair: (code, deviceName) =>
      request<PairedServer>("POST", "/bridge/pair", { code, device_name: deviceName }),
    status: () => request<ApiStatus>("POST", "/bridge/status", {}),
    projectList: () => request<ProjectList>("POST", "/bridge/verbs/project_list", {}),
    projectAdd: (args: ProjectAddArgs) => request<ProjectChanged>("POST", "/bridge/verbs/project_add", args),
    forget: async () => {
      await request<null>("POST", "/bridge/forget", {});
    },
  };
}
