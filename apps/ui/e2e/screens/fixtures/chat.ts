// The d25 test data of the chat: the two live sessions (S-1, S-2), the items of S-2 and the events of the
// stream. Owner: w3. The shell's `SESSION_LIST` (fixtures/shell.ts) carries S-1 and S-2 in its rows.
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
