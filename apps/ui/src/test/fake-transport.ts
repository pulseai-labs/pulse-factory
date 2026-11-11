// An in-memory transport for the unit tests. It never touches a timer, so a test that uses fake
// timers sees only the UI's own timers. `calls` lists every call in order.
import type {
  AgentRecord,
  ApiStatus,
  ArmActed,
  ChatFrame,
  ChatItemBody,
  ChatItemRecord,
  ChatOptionRecord,
  ChatRuntimeRecord,
  ChatSessionRecord,
  ClaimRecord,
  ConfirmCodeArgs,
  ConfirmIssued,
  DeviceAckArgs,
  DeviceAnswerArgs,
  DeviceArmArgs,
  DeviceClearArgs,
  DeviceRecord,
  ProjectAddArgs,
  ProjectChanged,
  ProjectList,
  ProjectRecord,
  RequestActed,
  RequestDetail,
  RequestFrame,
  RequestList,
  RequestSummary,
  RequestsArgs,
  RunList,
  RunSummary,
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
  SettingRecord,
  SettingScope,
  SettingValue,
  SettingsGetArgs,
  SettingsSetArgs,
  SettingsUnsetArgs,
  SettingsView,
  ShowArgs,
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
  | "requests"
  | "show"
  | "confirmCode"
  | "answer"
  | "clear"
  | "ack"
  | "settingsGet"
  | "settingsSet"
  | "settingsUnset"
  | "runList"
  | "arm"
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

/** The Claude Code runtime as the adapter declares it (docs/contracts/chat-adapter.md), plus `extra` options. */
export function claudeRuntime(extra: ChatOptionRecord[] = [], patch: Partial<ChatRuntimeRecord> = {}): ChatRuntimeRecord {
  return {
    id: "claude-code",
    options: [
      { id: "model", label: "Model", values: ["fake-model", "other-model"], default: "fake-model", free: true, live: true },
      { id: "effort", label: "Effort", values: ["low", "medium", "high", "max"], default: "high", free: false, live: true },
      {
        id: "permission_mode",
        label: "Permission mode",
        values: ["manual", "plan", "bypass"],
        default: "bypass",
        free: false,
        live: true,
      },
      { id: "artifacts", label: "Artifacts", values: ["on", "off"], default: "off", free: false, live: false },
      ...extra,
    ],
    mid_turn: "steer",
    ready: true,
    error: null,
    ...patch,
  };
}

/** The built-in Orchestrator, with the defaults a chat starts from. */
export function orchestrator(patch: Partial<AgentRecord> = {}): AgentRecord {
  return {
    id: "orchestrator",
    name: "Orchestrator",
    title: "Talks to a project's team",
    built_in: true,
    colour: "amber",
    head: "O",
    runtime: "claude-code",
    model: "fake-model",
    effort: "high",
    permission_mode: "bypass",
    skills: ["pulse-factory"],
    runtime_options: { artifacts: "off" },
    ...patch,
  };
}

/** One item of a chat's display log, as the stream sends it. */
export function item(sessionId: string, seq: number, body: ChatItemBody, patch: Partial<ChatItemRecord> = {}): ChatItemRecord {
  return {
    session_id: sessionId,
    seq,
    answers_seq: null,
    created_at: "2026-11-09T10:00:00Z",
    redacted_at: null,
    body,
    ...patch,
  };
}

export const SERVER = { id: "0".repeat(64), name: "fake-server" };

// ---- requests, the claim, runs and settings (r1.s4.w3) ----------------------------------------------

/** A request as `requests` and the stream show it. A stop request takes `answer` and `clear`. */
export function requestSummary(id: string, patch: Partial<RequestSummary> = {}): RequestSummary {
  const n = Number(id.replace("R-", ""));
  return {
    id,
    revision: 1,
    kind: "stop",
    status: "open",
    run: "R-1",
    unit: "u1",
    stage: "implement",
    blocks: [],
    opened_at: `2026-11-10T10:${String(n % 60).padStart(2, "0")}:00.000Z`,
    answered_at: null,
    reason: "the implementer asks a question",
    verbs: ["answer", "clear"],
    ...patch,
  };
}

/** A `merge_ack` request: its summary names the PR, the base and the head SHA. */
export function mergeAck(id: string, patch: Partial<RequestSummary> = {}): RequestSummary {
  return requestSummary(id, {
    kind: "merge_ack",
    unit: null,
    stage: "spine",
    reason: "spine merge waits on the claim's ack",
    verbs: ["ack"],
    pr: "pulseai-labs/example#9",
    base: "main",
    head_sha: "a".repeat(40),
    ...patch,
  });
}

export function claimRecord(patch: Partial<ClaimRecord> = {}): ClaimRecord {
  return {
    generation: 1,
    holder: "ann@host",
    device: null,
    revoked: false,
    claimed_at: "2026-11-10T10:00:00.000Z",
    ...patch,
  };
}

export function runSummary(id: string, patch: Partial<RunSummary> = {}): RunSummary {
  return {
    id,
    spine: "s1",
    workflow: "ossify-spine",
    state: "active",
    halted_reason: null,
    end_reason: null,
    open_requests: 0,
    waiting: null,
    ...patch,
  };
}

/** The frames about requests on the stream (docs/contracts/api.md, `GET /v1/stream`). */
export const requestFrames = {
  requests(requests: RequestSummary[], claim: ClaimRecord | null = null): RequestFrame {
    return { type: "requests", requests, claim };
  },
  request(request: RequestSummary): RequestFrame {
    return { type: "request", request };
  },
  claim(claim: ClaimRecord | null): RequestFrame {
    return { type: "claim", claim };
  },
};

/** The keys the fake server knows: the UI names none of them. */
const SETTING_DEFS: Array<{ key: string; built_in: SettingValue; applies: "next_turn" | "next_launch" }> = [
  { key: "chat.self_turn_cap", built_in: 4, applies: "next_turn" },
  { key: "chat.deny_commands", built_in: ["rm -rf"], applies: "next_launch" },
  { key: "chat.propose_per_min", built_in: 6, applies: "next_turn" },
];

interface FakeLayer {
  scope: SettingScope;
  value: SettingValue;
  set_by: string;
  set_at: string;
}

/** The frames of the chat stream (docs/contracts/api.md, `GET /v1/stream`). */
export const frames = {
  hello(sessions: ChatSessionRecord[], following: string | null = null, afterSeq = 0): ChatFrame {
    return { type: "hello", server: SERVER, sessions, following, after_seq: afterSeq };
  },
  session(record: ChatSessionRecord): ChatFrame {
    return { type: "session", session: record };
  },
  item(record: ChatItemRecord): ChatFrame {
    return { type: "item", item: record };
  },
  caught(sessionId: string, lastSeq: number): ChatFrame {
    return { type: "caught", session_id: sessionId, last_seq: lastSeq };
  },
  delta(sessionId: string, text: string): ChatFrame {
    return { type: "delta", session_id: sessionId, text };
  },
  toolRunning(sessionId: string, callId: string, tool: string): ChatFrame {
    return { type: "tool_running", session_id: sessionId, call_id: callId, tool };
  },
};

/** A stream the UI opened on the fake: its options and its handler. `emit` delivers to the open ones. */
export interface OpenStream {
  options: StreamOptions;
  onEvent: (event: StreamEvent) => void;
  closed: boolean;
}

export class FakeTransport implements Transport {
  readonly calls: Method[] = [];
  readonly requestsArgs: Array<RequestsArgs | undefined> = [];
  readonly showArgs: ShowArgs[] = [];
  readonly confirmArgs: ConfirmCodeArgs[] = [];
  /** The arguments of `answer`, `clear` and `ack`, in order, with the verb that took them. */
  readonly actArgs: Array<{ verb: "answer" | "clear" | "ack"; args: DeviceAnswerArgs | DeviceClearArgs | DeviceAckArgs }> = [];
  readonly settingsGetArgs: Array<SettingsGetArgs | undefined> = [];
  readonly settingsSetArgs: SettingsSetArgs[] = [];
  readonly settingsUnsetArgs: SettingsUnsetArgs[] = [];
  /** Every request the server holds, open or not. */
  requestRows: RequestSummary[] = [];
  /** The body each request's `show` answers. A request with none has an empty body. */
  bodies: Record<string, unknown> = {};
  /** The head branch a `merge_ack`'s confirm code shows. */
  headBranch = "spine/s1";
  claim: ClaimRecord | null = null;
  runs: RunSummary[] = [];
  /** The rows of the fake's settings table. */
  settingLayers: Record<string, FakeLayer[]> = {};
  private replays = new Set<Method>();
  private codeCounter = 0;
  readonly addArgs: ProjectAddArgs[] = [];
  readonly pairArgs: Array<{ code: string; deviceName: string }> = [];
  readonly sendArgs: SessionSendArgs[] = [];
  readonly startArgs: SessionStartArgs[] = [];
  readonly answerArgs: SessionAnswerArgs[] = [];
  readonly setArgs: SessionSetArgs[] = [];
  readonly interruptArgs: SessionInterruptArgs[] = [];
  readonly endArgs: SessionEndArgs[] = [];
  readonly armArgs: DeviceArmArgs[] = [];
  readonly streams: OpenStream[] = [];
  server: PairedServer | null;
  projects: ProjectRecord[];
  sessions: ChatSessionRecord[] = [];
  /** What `sessionList` answers as `runtimes` and `agents`. */
  runtimes: ChatRuntimeRecord[] = [];
  agents: AgentRecord[] = [];
  private notes = new Map<Method, string>();
  private failures = new Map<Method, { error: Error; left: number }>();

  constructor(opts: { server?: PairedServer | null; projects?: ProjectRecord[] } = {}) {
    this.server = opts.server === undefined ? { address: ADDRESS, device: device() } : opts.server;
    this.projects = opts.projects ?? [];
  }

  /** Fail the next `times` calls of a method (default: every call until `heal`). */
  fail(method: Method, kind: TransportErrorKind, message = `fake ${kind}`, times = Infinity, refusal?: string): void {
    this.failures.set(method, { error: new TransportError(kind, message, refusal), left: times });
  }

  /** The next answer of a method carries this `note`. */
  noteOnce(method: Method, text: string): void {
    this.notes.set(method, text);
  }

  private takeNote(method: Method): string | null {
    const note = this.notes.get(method) ?? null;
    this.notes.delete(method);
    return note;
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
      runtimes: [...this.runtimes],
      agents: [...this.agents],
      sessions,
    };
  }

  async sessionStart(args: SessionStartArgs): Promise<SessionStarted> {
    this.startArgs.push(args);
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

  private changed(method: Method, sessionId: string, patch: Partial<ChatSessionRecord> = {}): SessionChanged {
    const index = this.sessions.findIndex((s) => s.id === sessionId);
    if (index < 0) throw new TransportError("refused", `no chat session ${sessionId}`);
    const record = { ...(this.sessions[index] as ChatSessionRecord), ...patch };
    this.sessions[index] = record;
    return { schema_version: 1, replayed: false, session: record, note: this.takeNote(method) };
  }

  async sessionSend(args: SessionSendArgs): Promise<SessionChanged> {
    this.sendArgs.push(args);
    this.enter("sessionSend");
    return this.changed("sessionSend", args.session_id, { state: "working" });
  }

  async sessionAnswer(args: SessionAnswerArgs): Promise<SessionChanged> {
    this.answerArgs.push(args);
    this.enter("sessionAnswer");
    return this.changed("sessionAnswer", args.session_id, { state: "working" });
  }

  async sessionInterrupt(args: SessionInterruptArgs): Promise<SessionChanged> {
    this.interruptArgs.push(args);
    this.enter("sessionInterrupt");
    return this.changed("sessionInterrupt", args.session_id, { state: "idle" });
  }

  async sessionSet(args: SessionSetArgs): Promise<SessionChanged> {
    this.setArgs.push(args);
    this.enter("sessionSet");
    const current = this.sessions.find((s) => s.id === args.session_id);
    const patch: Partial<ChatSessionRecord> = {};
    if (args.model) patch.model = args.model;
    if (args.effort) patch.effort = args.effort;
    if (args.permission_mode) patch.permission_mode = args.permission_mode;
    if (args.options) {
      // An option that the runtime does not declare live waits in `pending_options`.
      const runtime = this.runtimes.find((r) => r.id === current?.runtime);
      const pending = { ...(current?.pending_options ?? {}) };
      const options = { ...(current?.runtime_options ?? {}) };
      for (const [id, value] of Object.entries(args.options)) {
        if (runtime?.options.find((o) => o.id === id)?.live === false) pending[id] = value;
        else options[id] = value;
      }
      patch.pending_options = pending;
      patch.runtime_options = options;
    }
    return this.changed("sessionSet", args.session_id, patch);
  }

  async sessionEnd(args: SessionEndArgs): Promise<SessionEnded> {
    this.endArgs.push(args);
    this.enter("sessionEnd");
    this.changed("sessionEnd", args.session_id, { state: "idle", ended_at: "2026-11-09T11:00:00Z" });
    return { replayed: false, session_id: args.session_id, note: null };
  }

  // ---- requests, the claim, runs ----

  /** Replace the requests the server holds, and the claim. */
  seedRequests(rows: RequestSummary[], claim: ClaimRecord | null = this.claim): void {
    this.requestRows = rows.map((r) => ({ ...r }));
    this.claim = claim;
  }

  /** The next `answer`, `clear` or `ack` answers `replayed: true` with every other field null and acts on nothing. */
  replayOnce(method: "answer" | "clear" | "ack"): void {
    this.replays.add(method);
  }

  async requests(args?: RequestsArgs): Promise<RequestList> {
    this.requestsArgs.push(args);
    this.enter("requests");
    const rows = this.requestRows.filter(
      (r) => (args?.all === true || r.status === "open") && (!args?.run || r.run === args.run),
    );
    return {
      schema_version: 1,
      open: rows.filter((r) => r.status === "open").length,
      requests: rows.map((r) => ({ ...r })),
      claim: this.claim,
    };
  }

  async show(args: ShowArgs): Promise<RequestDetail> {
    this.showArgs.push(args);
    this.enter("show");
    const row = this.requestRows.find((r) => r.id === args.id);
    if (!row) throw new TransportError("refused", `request ${args.id} is unknown`);
    return { schema_version: 1, ...row, body: this.bodies[args.id] ?? {}, answer: null, answer_generation: null };
  }

  /** The code the next arm `confirmCode` issues (a replay of a command ID answers `null`, as the server does). */
  nextCode = "pfc1-" + "a".repeat(32);
  private issued = new Map<string, ConfirmIssued>();

  async confirmCode(args: ConfirmCodeArgs): Promise<ConfirmIssued> {
    this.confirmArgs.push(args);
    this.enter("confirmCode");
    const subject = args.subject;
    if (subject.kind === "arm") {
      const first = this.issued.get(args.command_id);
      if (first) return { ...first, replayed: true, code: null };
      const issued: ConfirmIssued = {
        schema_version: 1,
        replayed: false,
        code: this.nextCode,
        expires_at: "2026-11-09T10:10:00Z",
        device: this.server?.device.id ?? "D-1",
        generation: null,
        holder: null,
        subject: { request: "", revision: 0, kind: subject.kind, run: "", unit: null, reason: "" },
        start: { proposal: subject.proposal, session: "S-1", project_id: "P-1", spine: "s1", plan_sha256: subject.plan_sha256 },
      };
      this.issued.set(args.command_id, issued);
      return issued;
    }
    const row = this.requestRows.find((r) => r.id === subject.request);
    if (!row) throw new TransportError("refused", `request ${subject.request} is unknown`);
    this.codeCounter += 1;
    const code = `pfc1-${this.codeCounter.toString(16).padStart(32, "0")}`;
    const merge = row.kind === "merge_ack";
    return {
      schema_version: 1,
      replayed: false,
      code,
      expires_at: "2026-11-10T10:02:00.000Z",
      device: this.server?.device.id ?? "D-1",
      generation: this.claim?.generation ?? null,
      holder: this.claim,
      subject: {
        request: row.id,
        revision: row.revision,
        kind: row.kind,
        run: row.run,
        unit: row.unit,
        reason: row.reason,
        ...(merge ? { pr: row.pr, base: row.base, head_branch: this.headBranch, head_sha: row.head_sha } : {}),
      },
    };
  }

  /** Move the claim to this device, as a device's act does. */
  private takeClaim(): ClaimRecord {
    const device = this.server?.device.id ?? "D-1";
    const claim: ClaimRecord = {
      generation: (this.claim?.generation ?? 0) + 1,
      holder: `device ${device}`,
      device,
      revoked: false,
      claimed_at: "2026-11-10T10:05:00.000Z",
    };
    this.claim = claim;
    return claim;
  }

  private acted(
    method: "answer" | "clear" | "ack",
    args: DeviceAnswerArgs | DeviceClearArgs | DeviceAckArgs,
  ): RequestActed {
    this.actArgs.push({ verb: method, args });
    this.enter(method);
    const device = this.server?.device.id ?? "D-1";
    const empty = { request: null, merged: null, head_sha: null, pr: null, base: null, outcome: null, resumed: null, hook: null, to_stage: null };
    if (this.replays.delete(method)) {
      return { schema_version: 1, replayed: true, verb: method, device, generation: null, ...empty };
    }
    const index = this.requestRows.findIndex((r) => r.id === args.request);
    if (index < 0) throw new TransportError("refused", `request ${args.request} is unknown`);
    const claim = this.takeClaim();
    const row = this.requestRows[index] as RequestSummary;
    const request: RequestSummary = { ...row, status: "answered", answered_at: new Date().toISOString(), verbs: [] };
    this.requestRows[index] = request;
    return {
      schema_version: 1,
      replayed: false,
      verb: method,
      device,
      generation: claim.generation,
      ...empty,
      request,
      ...(method === "ack"
        ? { merged: true, head_sha: row.head_sha ?? null, pr: row.pr ?? null, base: row.base ?? null, outcome: "landed" }
        : { resumed: true, hook: "none" }),
    };
  }

  async answer(args: DeviceAnswerArgs): Promise<RequestActed> {
    return this.acted("answer", args);
  }

  async clear(args: DeviceClearArgs): Promise<RequestActed> {
    return this.acted("clear", args);
  }

  async ack(args: DeviceAckArgs): Promise<RequestActed> {
    return this.acted("ack", args);
  }

  async runList(): Promise<RunList> {
    this.enter("runList");
    return { schema_version: 1, runs: this.runs.map((r) => ({ ...r })) };
  }

  // ---- settings ----

  private applies(scope: SettingScope, project: string | null, agent: string | null): boolean {
    switch (scope.kind) {
      case "server":
        return true;
      case "project":
        return project !== null && scope.project_id === project;
      case "agent":
        return agent !== null && scope.agent_id === agent;
      case "project_agent":
        return project !== null && agent !== null && scope.project_id === project && scope.agent_id === agent;
      default:
        return false;
    }
  }

  private static specificity(scope: SettingScope): number {
    return { project_agent: 3, agent: 2, project: 1, server: 0, built_in: -1 }[scope.kind];
  }

  private record(key: string, project: string | null, agent: string | null): SettingRecord {
    const def = SETTING_DEFS.find((d) => d.key === key);
    if (!def) throw new TransportError("refused", `unknown setting ${key}`);
    const chain = (this.settingLayers[key] ?? [])
      .filter((l) => this.applies(l.scope, project, agent))
      .sort((a, b) => FakeTransport.specificity(b.scope) - FakeTransport.specificity(a.scope));
    const winner = chain[0];
    return {
      key,
      value: winner ? winner.value : def.built_in,
      from: winner ? winner.scope : { kind: "built_in", project_id: null, agent_id: null },
      built_in: def.built_in,
      merge: "override",
      applies: def.applies,
      layers: chain.map((l) => ({ ...l })),
      masked: chain.slice(1).map((l) => ({ ...l })),
    };
  }

  async settingsGet(args?: SettingsGetArgs): Promise<SettingsView> {
    this.settingsGetArgs.push(args);
    this.enter("settingsGet");
    const project = args?.project_id ?? null;
    const agent = args?.agent_id ?? null;
    const keys = args?.key ? [args.key] : SETTING_DEFS.map((d) => d.key);
    return {
      schema_version: 1,
      project_id: project,
      agent_id: agent,
      settings: keys.map((k) => this.record(k, project, agent)),
    };
  }

  private static scopeOf(project: string | null | undefined, agent: string | null | undefined): SettingScope {
    const p = project ?? null;
    const a = agent ?? null;
    const kind = p && a ? "project_agent" : p ? "project" : a ? "agent" : "server";
    return { kind, project_id: p, agent_id: a };
  }

  async settingsSet(args: SettingsSetArgs): Promise<SettingChanged> {
    this.settingsSetArgs.push(args);
    this.enter("settingsSet");
    const scope = FakeTransport.scopeOf(args.project_id, args.agent_id);
    const layers = (this.settingLayers[args.key] ?? []).filter(
      (l) => !(l.scope.project_id === scope.project_id && l.scope.agent_id === scope.agent_id),
    );
    layers.push({ scope, value: args.value, set_by: this.server?.device.id ?? "D-1", set_at: "2026-11-10T10:06:00.000Z" });
    this.settingLayers[args.key] = layers;
    return {
      schema_version: 1,
      replayed: false,
      key: args.key,
      scope,
      value: args.value,
      resolved: this.record(args.key, scope.project_id, scope.agent_id),
    };
  }

  async settingsUnset(args: SettingsUnsetArgs): Promise<SettingChanged> {
    this.settingsUnsetArgs.push(args);
    this.enter("settingsUnset");
    const scope = FakeTransport.scopeOf(args.project_id, args.agent_id);
    this.settingLayers[args.key] = (this.settingLayers[args.key] ?? []).filter(
      (l) => !(l.scope.project_id === scope.project_id && l.scope.agent_id === scope.agent_id),
    );
    return {
      schema_version: 1,
      replayed: false,
      key: args.key,
      scope,
      value: null,
      resolved: this.record(args.key, scope.project_id, scope.agent_id),
    };
  }

  /** Deliver a request frame to every open stream. */
  requestFrame(frame: RequestFrame): void {
    this.emit({ event: "request_frame", frame });
  }

  async arm(args: DeviceArmArgs): Promise<ArmActed> {
    this.armArgs.push(args);
    this.enter("arm");
    return {
      schema_version: 1,
      replayed: false,
      device: this.server?.device.id ?? "D-1",
      proposal: args.proposal,
      run_id: "run-1",
      project_id: "P-1",
    };
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
    for (const stream of this.streams) this.emitTo(stream, event);
  }

  /** Deliver an event to one stream. It does nothing when that stream is closed. */
  emitTo(stream: OpenStream, event: StreamEvent): void {
    if (stream.closed) return;
    if (event.event === "closed") stream.closed = true;
    stream.onEvent(event);
  }

  /** The streams that are open now. */
  open(): OpenStream[] {
    return this.streams.filter((s) => !s.closed);
  }

  /** The newest stream that is open, or `undefined`. */
  current(): OpenStream | undefined {
    return this.open().at(-1);
  }

  /** Deliver a frame to the newest open stream. */
  frame(frame: ChatFrame): void {
    const stream = this.current();
    if (stream) this.emitTo(stream, { event: "frame", frame });
  }
}
