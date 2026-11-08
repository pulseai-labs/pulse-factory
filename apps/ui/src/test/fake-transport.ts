// An in-memory transport for the unit tests. It never touches a timer, so a test that uses fake
// timers sees only the UI's own timers. `calls` lists every call in order.
import type {
  ApiStatus,
  DeviceRecord,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
  ProjectRecord,
} from "@pulse-factory/api-client";
import {
  TransportError,
  type PairedServer,
  type Transport,
  type TransportErrorKind,
} from "../transport/types";

export type Method = "paired" | "pair" | "status" | "projectList" | "projectAdd" | "forget";

const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";

/** A pairing code with the layout of docs/contracts/api.md: `pf1-` and base32 of version, secret, hash, address. */
export function makeCode(address: string): string {
  const bytes = new Uint8Array([
    1,
    ...new Uint8Array(10).fill(7),
    ...new Uint8Array(32).fill(9),
    ...new TextEncoder().encode(address),
  ]);
  let bits = 0;
  let acc = 0;
  let out = "";
  for (const b of bytes) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += ALPHABET[(acc >> (bits - 5)) & 31];
      bits -= 5;
    }
    acc &= (1 << bits) - 1;
  }
  if (bits > 0) out += ALPHABET[(acc << (5 - bits)) & 31];
  return `pf1-${out}`;
}

export const ADDRESS = "100.90.203.21:7443";

export function device(id = "D-1", name = "MacBook"): DeviceRecord {
  return {
    id,
    name,
    paired_at: "2026-11-07T10:00:00Z",
    last_seen_at: null,
    last_peer: null,
    revoked_at: null,
  };
}

export function project(n: number, name = `project-${n}`): ProjectRecord {
  return {
    id: `P-${n}`,
    name,
    root: `/home/dev/${name}`,
    created_at: "2026-11-07T10:00:00Z",
    removed_at: null,
  };
}

export class FakeTransport implements Transport {
  readonly calls: Method[] = [];
  readonly addArgs: ProjectAddArgs[] = [];
  readonly pairArgs: Array<{ code: string; deviceName: string }> = [];
  server: PairedServer | null;
  projects: ProjectRecord[];
  private failures = new Map<Method, { error: Error; left: number }>();

  constructor(opts: { server?: PairedServer | null; projects?: ProjectRecord[] } = {}) {
    this.server = opts.server === undefined ? { address: ADDRESS, device: device() } : opts.server;
    this.projects = opts.projects ?? [];
  }

  /** Fail the next `times` calls of a method (default: every call until `heal`). */
  fail(method: Method, kind: TransportErrorKind, message = `fake ${kind}`, times = Infinity): void {
    this.failures.set(method, { error: new TransportError(kind, message), left: times });
  }

  heal(method?: Method): void {
    if (method) this.failures.delete(method);
    else this.failures.clear();
  }

  count(method: Method): number {
    return this.calls.filter((c) => c === method).length;
  }

  private enter(method: Method): void {
    this.calls.push(method);
    const f = this.failures.get(method);
    if (f && f.left > 0) {
      f.left -= 1;
      throw f.error;
    }
  }

  async paired(): Promise<PairedServer | null> {
    this.enter("paired");
    return this.server;
  }

  async pair(code: string, deviceName: string): Promise<PairedServer> {
    this.pairArgs.push({ code, deviceName });
    this.enter("pair");
    this.server = { address: ADDRESS, device: device("D-1", deviceName) };
    return this.server;
  }

  async status(): Promise<ApiStatus> {
    this.enter("status");
    return {
      schema_version: 1,
      version: "0.0.0-test",
      key_spki_sha256: "0".repeat(64),
      device: this.server?.device ?? null,
    };
  }

  async projectList(): Promise<ProjectList> {
    this.enter("projectList");
    return { schema_version: 1, projects: [...this.projects] };
  }

  async projectAdd(args: ProjectAddArgs): Promise<ProjectChanged> {
    this.addArgs.push(args);
    this.enter("projectAdd");
    const added = project(this.projects.length + 1, args.name ?? args.root.split("/").pop() ?? "project");
    const record = { ...added, root: args.root };
    this.projects.push(record);
    return { schema_version: 1, replayed: false, project: record };
  }

  async forget(): Promise<void> {
    this.enter("forget");
    this.server = null;
  }
}
