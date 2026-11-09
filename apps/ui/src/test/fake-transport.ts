// An in-memory transport for the unit tests. It never touches a timer, so a test that uses fake
// timers sees only the UI's own timers. `calls` lists every call in order.
import type {
  ApiStatus,
  ChatSessionRecord,
  DeviceRecord,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
  ProjectRecord,
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
  TransportError,
  type PairedServer,
  type SessionEnded,
  type SessionStarted,
  type StreamEvent,
  type StreamHandle,
  type StreamOptions,
  type Transport,
  type TransportErrorKind,
} from "../transport/types";

export type Method =
  | "paired"
  | "pair"
  | "status"
  | "projectList"
  | "projectAdd"
  | "forget"
  | "sessionList"
  | "sessionStart"
  | "sessionSend"
  | "sessionAnswer"
  | "sessionInterrupt"
  | "sessionSet"
  | "sessionEnd"
  | "openStream";

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
    kind: "git",
    remote_key: null,
    kind_error: null,
  };
}

/** A chat session record with every field of `ChatSessionRecord`. */
export function chatSession(id: string, patch: Partial<ChatSessionRecord> = {}): ChatSessionRecord {
  return {
    id,
    project_id: "P-1",
    project_name: "project-1",
    worktree_id: "W-1",
    worktree_dir: "/home/dev/project-1",
    branch: "main",
    detached: false,
    agent_id: "orchestrator",
    runtime: "claude-code",
    runtime_session_ref: null,
    state: "idle",
    context_lost: null,
    process: null,
    model: "fake-model",
    effort: "high",
    permission_mode: "bypass",
    runtime_options: {},
    pending_options: {},
    usage: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 0 },
    last_turn_at: null,
    cache_ttl_seconds: null,
    context: null,
    created_at: "2026-11-09T10:00:00Z",
    updated_at: "2026-11-09T10:00:00Z",
    ended_at: null,
    ...patch,
  };
}

/** A stream the UI opened on the fake: its options and its handler. `emit` delivers to the open ones. */
export interface OpenStream {
  options: StreamOptions;
  onEvent: (event: StreamEvent) => void;
  closed: boolean;
}

export class FakeTransport implements Transport {
  readonly calls: Method[] = [];
  readonly addArgs: ProjectAddArgs[] = [];
  readonly pairArgs: Array<{ code: string; deviceName: string }> = [];
  readonly sendArgs: SessionSendArgs[] = [];
  readonly streams: OpenStream[] = [];
  server: PairedServer | null;
  projects: ProjectRecord[];
  sessions: ChatSessionRecord[] = [];
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

  async sessionList(args: SessionListArgs = {}): Promise<SessionList> {
    this.enter("sessionList");
    const sessions = this.sessions.filter(
      (s) => (args.all === true || s.ended_at === null) && (!args.project_id || s.project_id === args.project_id),
    );
    return {
      schema_version: 1,
      server: { id: "0".repeat(64), name: "fake-server" },
      runtimes: [],
      agents: [],
      sessions,
    };
  }

  async sessionStart(args: SessionStartArgs): Promise<SessionStarted> {
    this.enter("sessionStart");
    const n = this.sessions.length + 1;
    const project = this.projects.find((p) => p.id === args.project_id);
    const record = chatSession(`S-${n}`, {
      project_id: args.project_id,
      project_name: project?.name ?? args.project_id,
      worktree_id: `W-${n}`,
      worktree_dir: project?.root ?? "/home/dev/project-1",
      agent_id: args.agent_id ?? "orchestrator",
    });
    this.sessions.push(record);
    return {
      replayed: false,
      session_id: record.id,
      worktree_id: record.worktree_id,
      worktree_dir: record.worktree_dir,
      note: null,
    };
  }

  private changed(sessionId: string, patch: Partial<ChatSessionRecord> = {}): SessionChanged {
    const index = this.sessions.findIndex((s) => s.id === sessionId);
    if (index < 0) throw new TransportError("refused", `no chat session ${sessionId}`);
    const record = { ...(this.sessions[index] as ChatSessionRecord), ...patch };
    this.sessions[index] = record;
    return { schema_version: 1, replayed: false, session: record, note: null };
  }

  async sessionSend(args: SessionSendArgs): Promise<SessionChanged> {
    this.sendArgs.push(args);
    this.enter("sessionSend");
    return this.changed(args.session_id, { state: "working" });
  }

  async sessionAnswer(args: SessionAnswerArgs): Promise<SessionChanged> {
    this.enter("sessionAnswer");
    return this.changed(args.session_id, { state: "working" });
  }

  async sessionInterrupt(args: SessionInterruptArgs): Promise<SessionChanged> {
    this.enter("sessionInterrupt");
    return this.changed(args.session_id, { state: "idle" });
  }

  async sessionSet(args: SessionSetArgs): Promise<SessionChanged> {
    this.enter("sessionSet");
    const patch: Partial<ChatSessionRecord> = {};
    if (args.model) patch.model = args.model;
    if (args.effort) patch.effort = args.effort;
    if (args.permission_mode) patch.permission_mode = args.permission_mode;
    return this.changed(args.session_id, patch);
  }

  async sessionEnd(args: SessionEndArgs): Promise<SessionEnded> {
    this.enter("sessionEnd");
    this.changed(args.session_id, { state: "idle", ended_at: "2026-11-09T11:00:00Z" });
    return { replayed: false, session_id: args.session_id, note: null };
  }

  async openStream(options: StreamOptions, onEvent: (event: StreamEvent) => void): Promise<StreamHandle> {
    this.enter("openStream");
    const stream: OpenStream = { options, onEvent, closed: false };
    this.streams.push(stream);
    return {
      close: async () => {
        stream.closed = true;
      },
    };
  }

  /** Deliver an event to every open stream. A `closed` event ends them: nothing follows it. */
  emit(event: StreamEvent): void {
    for (const stream of this.streams) {
      if (stream.closed) continue;
      if (event.event === "closed") stream.closed = true;
      stream.onEvent(event);
    }
  }
}
