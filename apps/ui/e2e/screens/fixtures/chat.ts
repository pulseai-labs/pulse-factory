// The d25 test data of the chat: the two live sessions (S-1, S-2), the items of S-2 and the events of the
// stream. Owner: r1.s6.w2 (r1.s5.w3 before it). The shell's `SESSION_LIST` (fixtures/shell.ts) carries S-1 and S-2 in its rows.
import type { ArmSummary, ChatFrame, ChatItemBody, ChatItemRecord, ChatSessionRecord } from "@pulse-factory/api-client";
import { chatSession, frames, item } from "../../../src/test/fake-transport";
import type { StreamEvent } from "../../../src/transport/types";
import { REQUESTS_FRAME } from "./needs-you";
import type { Verbs } from "./verbs";

const PLAN_SHA = "3f".repeat(32);

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
  effort: "xhigh",
  permission_mode: "manual",
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
  // The two merging tracks of the `ossify-spine` workflow, and the two sentences the server writes for them.
  merges: [
    { track: "item", base: "spine/r1.s5", sentence: "item PRs merge into spine/r1.s5 on green checks with no ack" },
    { track: "spine", base: "main", sentence: "the spine PR merges into main only on the claim's ack" },
  ],
  base_pushes: [{ track: "main", stage: "plan", dir: "/home/dev/pulse-factory", branch: "spine/r1.s5" }],
  replaces: null,
  paper: "paper-spine-d25",
  forge_repo: "pulseai-labs/pulse-factory",
  default_branch: "main",
};

const text = (value: string) => ({ text: value, bytes: value.length, truncated: false });
const input = (fields: Record<string, string>) => text(JSON.stringify(fields));

const FIRST_QUESTION = "Which screens come first?";

/**
 * The items of S-2, in `seq` order (11a): an interrupted turn, the idle release, a turn with two tool calls
 * and a Markdown reply with inline code, a single-select question that is answered (at 10), the start card,
 * and a single-select question that is open. The turn that holds the card has its result at 13.
 */
const BODIES: Array<[body: ChatItemBody, at: string, answers?: number]> = [
  [{ kind: "operator_message", text: "Scan every frame of the canvas and list the screens the Mac app has." }, "2026-11-08T13:10:00Z"],
  [
    {
      kind: "tool_call",
      call_id: "call-0",
      tool: "Glob",
      input: input({ pattern: "canvas/**/*.html" }),
      output: text("canvas/r1-screens.dc.html"),
      is_error: false,
      finished: true,
    },
    "2026-11-08T13:11:00Z",
  ],
  [
    {
      kind: "turn_result",
      cause: "interrupt",
      usage: { input_tokens: 4000, output_tokens: 300, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 21000 },
      is_error: true,
      stop: "interrupted",
      duration_ms: 54_000,
      cache_ttl_seconds: 3600,
    },
    "2026-11-08T13:12:00Z",
  ],
  [{ kind: "notice", code: "idle_release", text: "The chat process was released after 30 idle minutes." }, "2026-11-08T13:42:00Z"],
  [{ kind: "operator_message", text: "Plan r1.s5 with me: every screen of the Mac app should match the approved canvas." }, "2026-11-08T14:02:00Z"],
  [
    {
      kind: "tool_call",
      call_id: "call-1",
      tool: "Read",
      input: input({ file_path: "docs/specs/r1/r1.s5/SPINE.md" }),
      output: text("# r1.s5: the MacBook app's screens match the approved canvas\n\nClass: bone. Release: r1."),
      is_error: false,
      finished: true,
    },
    "2026-11-08T14:03:00Z",
  ],
  [
    {
      kind: "tool_call",
      call_id: "call-2",
      tool: "Grep",
      input: input({ pattern: "\"screens\" in docs/specs/r1" }),
      output: text("14 hits"),
      is_error: false,
      finished: true,
    },
    "2026-11-08T14:03:30Z",
  ],
  [
    {
      kind: "assistant_text",
      text: "I read the canvas and the gap walk.\n\nBoth changes touch `apps/ui`. **Round 1** gives the tokens and the shared components.\n- Round 2 restyles the shell.\n- Round 3 restyles the chat, Needs you and Projects.\n\nOne question before I plan it.",
    },
    "2026-11-08T14:04:00Z",
  ],
  [
    {
      kind: "question_request",
      request_id: "Q-30",
      items: [
        {
          text: FIRST_QUESTION,
          header: "Order",
          options: [
            { label: "Chat first", description: "The screen the operator uses most." },
            { label: "Projects first", description: "The smallest screen." },
          ],
          multi_select: false,
        },
      ],
    },
    "2026-11-08T14:05:00Z",
  ],
  [{ kind: "question_answer", outcome: "answered", reason: null, answers: { [FIRST_QUESTION]: "Chat first" } }, "2026-11-08T14:08:00Z", 9],
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
  [
    {
      kind: "turn_result",
      cause: "result",
      usage: { input_tokens: 90000, output_tokens: 9000, cache_read_tokens: 800000, cache_write_tokens: 40000, cost_micros: 2100000 },
      is_error: false,
      stop: "end_turn",
      duration_ms: 72_000,
      cache_ttl_seconds: 3600,
    },
    "2026-11-08T14:41:30Z",
  ],
  [
    {
      kind: "question_request",
      request_id: "Q-31",
      items: [
        {
          text: "The store schema spine and the agents spine both want Release 1. Which goes first?",
          header: "",
          options: [
            { label: "Store schema first", description: "The agents spine waits one round and builds on the new sessions table." },
            { label: "Agents first", description: "The store schema spine rebases onto agent_id when it starts." },
          ],
          multi_select: false,
        },
      ],
    },
    "2026-11-08T14:44:00Z",
  ],
];

export const S2_ITEMS: ChatItemRecord[] = BODIES.map(([body, at, answers], i) =>
  item("S-2", i + 1, body, { created_at: at, answers_seq: answers ?? null }),
);

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

/** No verb of its own: the chat reads the stream and `session_list`. */
export const verbs: Verbs = {};
