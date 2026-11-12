// The test data of d25: what the fixture server answers for the ten screens. It is typed with the
// `@pulse-factory/api-client` types and built with the unit tests' builders (`src/test/fake-transport.ts`),
// so `npm run typecheck` checks it against the wire types. It is byte-stable: fixed IDs, fixed times on
// 2026-11-08 between 09:00 and 14:47 UTC, fixed costs and SHAs. It holds no token and no pairing code.
// Round 2 may add data for an element it adds; it never changes a value another screen shows without
// updating that screen's screenshot in the same PR.
import type {
  ApiStatus,
  ArmSummary,
  ChatFrame,
  ChatItemBody,
  ChatItemRecord,
  ChatSessionRecord,
  ProjectList,
  RequestDetail,
  RequestFrame,
  RequestList,
  RequestSummary,
  RunList,
  SessionList,
  SettingsView,
} from "@pulse-factory/api-client";
import {
  ADDRESS,
  SERVER,
  chatSession,
  claimRecord,
  claudeRuntime,
  device,
  frames,
  item,
  mergeAck,
  orchestrator,
  project,
  requestFrames,
  requestSummary,
  runSummary,
} from "../../src/test/fake-transport";
import type { PairedServer, StreamEvent } from "../../src/transport/types";

const SERVER_NAME = "draco-desk";
const THIS_DEVICE = { ...device("D-1", "MacBook"), paired_at: "2026-11-08T09:00:00Z", last_seen_at: "2026-11-08T14:46:00Z" };
const PLAN_SHA = "3f".repeat(32);
const HEAD_SHA = "9c1e7a4b20d5f86e3a71c0b4d2e95f18a6b3c7d0";

export const PAIRED: PairedServer = { address: ADDRESS, device: THIS_DEVICE, server: { ...SERVER, name: SERVER_NAME } };

export const STATUS: ApiStatus = {
  schema_version: 1,
  version: "0.1.0",
  key_spki_sha256: "b7".repeat(32),
  device: THIS_DEVICE,
  server: { ...SERVER, name: SERVER_NAME },
};

export const PROJECTS: ProjectList = {
  schema_version: 1,
  projects: [
    { ...project(1, "pulse-factory"), created_at: "2026-11-08T09:00:00Z" },
    { ...project(2, "pilot-fourseat"), created_at: "2026-11-08T09:02:00Z" },
  ],
};

const RUNTIME = claudeRuntime([], {
  options: [
    { id: "model", label: "Model", values: ["claude-opus-5-5", "claude-sonnet-5-5"], default: "claude-opus-5-5", free: true, live: true },
    { id: "effort", label: "Effort", values: ["low", "medium", "high", "max"], default: "high", free: false, live: true },
    { id: "permission_mode", label: "Permission mode", values: ["manual", "plan", "bypass"], default: "bypass", free: false, live: true },
  ],
});

const AGENT = orchestrator({
  title: "Plans spines with you and arms workflows",
  model: "claude-opus-5-5",
  skills: ["pulse-factory", "ossify"],
  runtime_options: {},
});

const USAGE = { input_tokens: 182340, output_tokens: 20418, cache_read_tokens: 1204551, cache_write_tokens: 88213, cost_micros: 4182000 };

/** The chat the spec opens (S-2): the Orchestrator in `pulse-factory`, waiting on the operator. */
export const S2: ChatSessionRecord = chatSession("S-2", {
  project_id: "P-1",
  project_name: "pulse-factory",
  worktree_id: "W-2",
  worktree_dir: "/home/dev/pulse-factory",
  branch: "spine/r1.s5",
  state: "needs_you",
  model: "claude-opus-5-5",
  effort: "high",
  permission_mode: "bypass",
  usage: USAGE,
  last_turn_at: "2026-11-08T14:41:00Z",
  cache_ttl_seconds: 3600,
  context: { used_tokens: 62000, max_tokens: 200000 },
  created_at: "2026-11-08T09:12:00Z",
  updated_at: "2026-11-08T14:41:00Z",
});

/** A second live session (S-1): the Orchestrator in `pilot-fourseat`, working. */
export const S1: ChatSessionRecord = chatSession("S-1", {
  project_id: "P-2",
  project_name: "pilot-fourseat",
  worktree_id: "W-1",
  worktree_dir: "/home/dev/pilot-fourseat",
  branch: "main",
  state: "working",
  model: "claude-opus-5-5",
  usage: { ...USAGE, input_tokens: 54012, output_tokens: 6120, cost_micros: 1204000 },
  last_turn_at: "2026-11-08T14:45:00Z",
  cache_ttl_seconds: 300,
  context: { used_tokens: 31000, max_tokens: 200000 },
  created_at: "2026-11-08T09:05:00Z",
  updated_at: "2026-11-08T14:45:00Z",
});

export const SESSION_LIST: SessionList = {
  schema_version: 1,
  server: { ...SERVER, name: SERVER_NAME },
  runtimes: [RUNTIME],
  agents: [AGENT],
  sessions: [S1, S2],
};

const SUMMARY: ArmSummary = {
  schema_version: 1,
  spine: "r1.s5",
  items: 4,
  workflow: { name: "ossify-spine", version: 2 },
  roles: [
    { name: "implementer", runtime: "claude-code", model: "claude-sonnet-5-5" },
    { name: "reviewer", runtime: "claude-code", model: "claude-opus-5-5" },
  ],
  caps: { model_sessions: 24, cold_builds: 2, seeds: [{ id: "w1", max: 2 }], relaunch: "once", loops: [{ track: "main", from: "review", to: "fix", max: 3 }] },
  plugins: [{ name: "ossify", version: "1.39.0" }],
  most_launches: 18,
  merges: [{ track: "main", base: "main", sentence: "the spine merges on the operator's ack" }],
  base_pushes: [{ track: "main", stage: "plan", dir: "/home/dev/pulse-factory", branch: "spine/r1.s5" }],
  replaces: null,
  paper: "paper-spine-d25",
  forge_repo: "pulseai-labs/pulse-factory",
  default_branch: "main",
};

const text = (value: string) => ({ text: value, bytes: value.length, truncated: false });

/** The items of S-2, in `seq` order. The question at 4 is answered by the item at 5. */
const BODIES: Array<[body: ChatItemBody, at: string, answers?: number]> = [
  [{ kind: "operator_message", text: "Plan r1.s5 with me: every screen of the Mac app should match the approved canvas." }, "2026-11-08T14:30:00Z"],
  [
    {
      kind: "assistant_text",
      text: "I read the canvas and the gap walk.\n\n**Round 1** gives the tokens and the shared components.\n- Round 2 restyles the screens.\n- `d25` holds the screenshots.\n\nOne question before I plan it.",
    },
    "2026-11-08T14:31:00Z",
  ],
  [
    {
      kind: "tool_call",
      call_id: "call-1",
      tool: "Read",
      input: text("docs/specs/r1/r1.s5/SPINE.md"),
      output: text("# r1.s5: the MacBook app's screens match the approved canvas\n\nClass: bone. Release: r1."),
      is_error: false,
      finished: true,
    },
    "2026-11-08T14:32:00Z",
  ],
  [
    {
      kind: "question_request",
      request_id: "Q-1",
      items: [
        {
          text: "Which screens come first?",
          header: "Order",
          options: [
            { label: "Chat first", description: "The screen the operator uses most." },
            { label: "Projects first", description: "The smallest screen." },
          ],
          multi_select: false,
        },
      ],
    },
    "2026-11-08T14:33:00Z",
  ],
  [{ kind: "question_answer", outcome: "answered", reason: null, answers: { "Which screens come first?": "Chat first" } }, "2026-11-08T14:34:00Z", 4],
  [{ kind: "assistant_text", text: "Chat first, then Projects. Here is the plan to arm." }, "2026-11-08T14:36:00Z"],
  [
    {
      kind: "start_card",
      proposal: "sp-1",
      project_id: "P-1",
      project_name: "pulse-factory",
      plan_sha256: PLAN_SHA,
      summary: SUMMARY,
      params: { goal: "restyle to the canvas" },
      ledger: "docs/specs/r1/r1.s5/autopilot-ledger.md",
      plugin_roots: [],
    },
    "2026-11-08T14:41:00Z",
  ],
];

export const S2_ITEMS: ChatItemRecord[] = BODIES.map(([body, at, answers], i) =>
  item("S-2", i + 1, body, { created_at: at, answers_seq: answers ?? null }),
);

const OPEN_STOP: RequestSummary = requestSummary("R-5", {
  run: "r0-s19-1",
  unit: "w2",
  stage: "implement",
  opened_at: "2026-11-08T13:58:00.000Z",
  reason: "the implementer asks a question",
});
const OPEN_ACK: RequestSummary = mergeAck("R-6", {
  run: "r0-s19-1",
  opened_at: "2026-11-08T14:20:00.000Z",
  pr: "pulseai-labs/pulse-factory#80",
  base: "main",
  head_sha: HEAD_SHA,
});
const answered = (id: string, kind: string, at: string, patch: Partial<RequestSummary> = {}): RequestSummary =>
  requestSummary(id, { kind, status: "answered", revision: 2, run: "r0-s18-1", opened_at: "2026-11-08T09:20:00.000Z", answered_at: at, verbs: [], ...patch });

export const REQUESTS: RequestSummary[] = [
  answered("R-2", "stop", "2026-11-08T10:12:00.000Z"),
  answered("R-3", "gaps", "2026-11-08T11:30:00.000Z"),
  answered("R-4", "merge_ack", "2026-11-08T12:44:00.000Z", { unit: null, stage: "spine", pr: "pulseai-labs/pulse-factory#79", base: "main", head_sha: "1d".repeat(20) }),
  OPEN_STOP,
  OPEN_ACK,
];

const CLAIM = claimRecord({ generation: 3, holder: "D-2", device: "D-2", claimed_at: "2026-11-08T12:44:00.000Z" });

export const REQUEST_LIST: RequestList = { schema_version: 1, open: 2, requests: REQUESTS, claim: CLAIM };

/** What `show` answers, by request ID. Only an open stop request has a body the page reads. */
export const DETAILS: Record<string, RequestDetail> = Object.fromEntries(
  REQUESTS.map((r): [string, RequestDetail] => [
    r.id,
    {
      schema_version: 1,
      body:
        r.id === "R-5"
          ? {
              stop_text: "AC-8's command exits 0 at the base only because the script does not exist yet. May the RED gate skip it?",
              gaps: [{ section: "AC-8", question: "Is a mutated-bundle run that fails with rc 127 the expected mutation failure?", severity: "blocking" }],
            }
          : {},
      answer: r.status === "answered" ? { text: "Yes." } : null,
      answer_generation: r.status === "answered" ? 3 : null,
      id: r.id,
      revision: r.revision,
      kind: r.kind,
      status: r.status,
      run: r.run,
      unit: r.unit,
      stage: r.stage,
      blocks: r.blocks,
      opened_at: r.opened_at,
      answered_at: r.answered_at,
      reason: r.reason,
      verbs: r.verbs,
      ...(r.pr !== undefined ? { pr: r.pr } : {}),
      ...(r.base !== undefined ? { base: r.base } : {}),
      ...(r.head_sha !== undefined ? { head_sha: r.head_sha } : {}),
    },
  ]),
);

export const RUNS: RunList = {
  schema_version: 1,
  runs: [
    runSummary("r0-s18-1", { spine: "r1.s4", state: "ended", end_reason: "the spine merged" }),
    runSummary("r0-s19-1", { spine: "r1.s5", state: "active", open_requests: 2, waiting: "you answer R-5 and ack R-6" }),
  ],
};

const SERVER_SCOPE = { kind: "server", project_id: null, agent_id: null } as const;
const BUILT_IN_SCOPE = { kind: "built_in", project_id: null, agent_id: null } as const;

export const SETTINGS: SettingsView = {
  schema_version: 1,
  project_id: null,
  agent_id: null,
  settings: [
    { key: "chat.deny_commands", value: ["rm -rf", "git push --force"], from: SERVER_SCOPE, built_in: ["rm -rf"], merge: "override", applies: "next_launch",
      layers: [{ scope: SERVER_SCOPE, value: ["rm -rf", "git push --force"], set_by: "cli", set_at: "2026-11-08T09:30:00Z" }], masked: [] },
    { key: "chat.propose_per_min", value: 6, from: BUILT_IN_SCOPE, built_in: 6, merge: "override", applies: "next_turn", layers: [], masked: [] },
    { key: "chat.self_turn_cap", value: 4, from: BUILT_IN_SCOPE, built_in: 4, merge: "override", applies: "next_turn", layers: [], masked: [] },
  ],
};

/** The request frame that opens every stream: the snapshot of the requests and the claim. */
const REQUESTS_FRAME: RequestFrame = requestFrames.requests(REQUESTS, CLAIM);

const frameEvent = (frame: ChatFrame): StreamEvent => ({ event: "frame", frame });

/** The events of `GET /bridge/stream`, in order: the overview, or one chat after `after`. */
export function streamEvents(session: string | null, after: number): StreamEvent[] {
  const sessions = [S1, S2];
  const out: StreamEvent[] = [frameEvent(frames.hello(sessions, session, after)), { event: "request_frame", frame: REQUESTS_FRAME }];
  if (session === "S-2") {
    for (const record of S2_ITEMS) if (record.seq > after) out.push(frameEvent(frames.item(record)));
    out.push(frameEvent(frames.caught("S-2", S2_ITEMS.length)));
  }
  return out;
}

export const UNREACHABLE_MESSAGE = `cannot reach ${ADDRESS}`;
