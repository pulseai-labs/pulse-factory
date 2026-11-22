// The d25 test data of the Needs you page and the dock: the requests, the claim and the details that `show`
// answers. Owner: w4; renewed by r2.s11.w5 with the decision blocks of the frames 12a to 12f. The shell's counts
// and the stream's request frame read the base requests too. The cases the base does not hold (`CASES`) are not
// in the base list: one more open request would change the rail's count on every other owner's screenshot, so a
// Needs you test adds the ones it shows through `overlay` (`screens.spec.ts`, `needsYouOverlay`).
//
// Every block holds the controller's real summary sentence (`summary_of`) and stored roles and stages, never
// the frames' examples. The data is typed with the generated types, so `npm run typecheck` checks it.
import type { Decision, DecisionOption, NoticeRecord, RequestDetail, RequestFrame, RequestList, RequestSummary } from "@pulse-factory/api-client";
import { claimRecord, mergeAck, requestFrames, requestSummary } from "../../../src/test/fake-transport";
import type { Verbs } from "./verbs";

const HEAD_SHA = "9c1e7a4b20d5f86e3a71c0b4d2e95f18a6b3c7d0";
const PR = "pulseai-labs/pulse-factory#80";
const PR_URL = "https://github.com/pulseai-labs/pulse-factory/pull/80";
const RUN = "r0-s19-1";
const PROJECT = "P-1";

/** One request of the fixtures: its row (`requests`) and what `show` answers. */
export interface Case {
  request: RequestSummary;
  detail: RequestDetail;
}

const seat = (role: string, unit: string) => ({ kind: "seat", role, unit }) as const;

function block(patch: Partial<Decision> & Pick<Decision, "cause" | "summary">): Decision {
  return {
    v: 1,
    options: [],
    refs: { project: PROJECT, run: RUN },
    attempt: 1,
    asked_by: { kind: "controller" },
    takes_text: true,
    ...patch,
  };
}

const answerOption = (id: string, label: string, by: string, patch: Partial<DecisionOption> = {}): DecisionOption => ({
  id,
  label,
  detail: `Sends the answer of ${id} to the seat.`,
  effect: { verb: "answer", text: label },
  by,
  ...patch,
});

const resumeOption = (id: string, label: string, detail: string, to?: string, by = "controller"): DecisionOption => ({
  id,
  label,
  detail,
  effect: to === undefined ? { verb: "resume" } : { verb: "resume", to },
  by,
});

/** A request row with the block's cause and summary, as `requests` shows them. */
function row(id: string, decision: Decision | undefined, patch: Partial<RequestSummary>): RequestSummary {
  return requestSummary(id, {
    run: RUN,
    ...(decision ? { cause: decision.cause, summary: decision.summary } : {}),
    ...patch,
  });
}

function make(request: RequestSummary, decision: Decision | undefined, body: Record<string, unknown>, answer: unknown = null): Case {
  const answered = request.status === "answered";
  return {
    request,
    detail: {
      schema_version: 1,
      ...request,
      body,
      answer,
      answer_generation: answered ? 3 : null,
      ...(decision ? { decision } : {}),
    },
  };
}

// ---- the base: what the rail counts and the dock shows ----

const STOP_OPTIONS = [
  "Yes: skip AC-8 at the RED gate, because the script does not exist yet.",
  "No: write a stub of the script first, so AC-8 reads RED.",
];

/** R-5: a seat's question with two options, the second recommended by the seat. */
const R5_BLOCK = block({
  cause: "stop_file",
  summary: "Unit w2 stopped at stage implement and wrote a stop file.",
  options: [answerOption("s1", STOP_OPTIONS[0] as string, "seat"), answerOption("s2", STOP_OPTIONS[1] as string, "seat")],
  recommended: { option: "s2", by: "seat", why: "The seat marked this option." },
  refs: { project: PROJECT, run: RUN, unit: "w2", stage: "implement" },
  asked_by: seat("implementer", "w2"),
});
const OPEN_STOP = row("R-5", R5_BLOCK, {
  unit: "w2",
  stage: "implement",
  opened_at: "2026-11-08T13:58:00.000Z",
  reason: "the implementer asks a question",
  blocks: ["w2"],
});

/** R-6: the merge approval. Its block holds `approve` and no recommendation. */
const R6_BLOCK = block({
  cause: "merge_waits_ack",
  summary: "the PR pulseai-labs/pulse-factory#80 of unit r1.s5 is ready to merge. It waits for your ack of head 9c1e7a4b20d5.",
  options: [{ id: "approve", label: "Approve merge", detail: "Merges the PR at the head the code binds.", effect: { verb: "ack" }, by: "controller" }],
  refs: { project: PROJECT, run: RUN, stage: "merge", pr: PR },
  attempt: 0,
  takes_text: false,
});
const OPEN_ACK = mergeAck("R-6", {
  run: RUN,
  opened_at: "2026-11-08T14:20:00.000Z",
  pr: PR_URL,
  base: "main",
  head_sha: HEAD_SHA,
  blocks: ["w4", "w5"],
  cause: R6_BLOCK.cause,
  summary: R6_BLOCK.summary,
});

const answered = (id: string, kind: string, at: string, patch: Partial<RequestSummary> = {}): RequestSummary =>
  requestSummary(id, { kind, status: "answered", revision: 2, run: "r0-s18-1", opened_at: "2026-11-08T09:20:00.000Z", answered_at: at, verbs: [], ...patch });

const R2_BLOCK = block({
  cause: "stop_file",
  summary: "Unit w1 stopped at stage implement and wrote a stop file.",
  options: [
    answerOption("s1", "Yes, split it.", "seat"),
    answerOption("s2", "No, keep it as one unit.", "seat"),
  ],
  refs: { project: PROJECT, run: "r0-s18-1", unit: "w1", stage: "implement" },
  asked_by: seat("implementer", "w1"),
});
const R3_BLOCK = block({
  cause: "park_file",
  summary: "Unit w2 parked at stage plan because the seat wrote a park file.",
  refs: { project: PROJECT, run: "r0-s18-1", unit: "w2", stage: "plan" },
  asked_by: seat("round_planner", "w2"),
});
const R4_BLOCK = block({
  cause: "merge_waits_ack",
  summary: "the PR pulseai-labs/pulse-factory#79 of unit r1.s4 is ready to merge. It waits for your ack of head 1d1d1d1d1d1d.",
  options: [{ id: "approve", label: "Approve merge", detail: "Merges the PR at the head the code binds.", effect: { verb: "ack" }, by: "controller" }],
  refs: { project: PROJECT, run: "r0-s18-1", stage: "spine", pr: "pulseai-labs/pulse-factory#79" },
  attempt: 0,
  takes_text: false,
});
const R10_BLOCK = block({
  cause: "stalled",
  summary: "The seat of unit w3 stalled at stage implement (attempt 2) after its one relaunch.",
  options: [resumeOption("resume", "Resume", "Resumes the unit at its stage.")],
  recommended: { option: "resume", by: "controller", why: "A stall is often transient: a new launch may go on." },
  refs: { project: PROJECT, run: "r0-s18-1", unit: "w3", stage: "implement" },
  attempt: 2,
  about: { role: "implementer", unit: "w3" },
});

const R2 = answered("R-2", "stop", "2026-11-08T10:12:00.000Z", { unit: "w1", cause: R2_BLOCK.cause, summary: R2_BLOCK.summary });
const R3 = answered("R-3", "gaps", "2026-11-08T11:30:00.000Z", { unit: "w2", stage: "plan", cause: R3_BLOCK.cause, summary: R3_BLOCK.summary });
const R4 = answered("R-4", "merge_ack", "2026-11-08T12:44:00.000Z", {
  unit: null,
  stage: "spine",
  pr: "pulseai-labs/pulse-factory#79",
  base: "main",
  head_sha: "1d".repeat(20),
  cause: R4_BLOCK.cause,
  summary: R4_BLOCK.summary,
});
const R10 = answered("R-10", "stall", "2026-11-08T13:41:00.000Z", { unit: "w3", stage: "implement", cause: R10_BLOCK.cause, summary: R10_BLOCK.summary });

export const REQUESTS: RequestSummary[] = [R2, R3, R4, R10, OPEN_STOP, OPEN_ACK];

/** A CLI claim: the answered rows of the generation it holds read "CLI · claim session C-2". */
const CLAIM = claimRecord({ generation: 3, holder: "session C-2", device: null, claimed_at: "2026-11-08T09:30:00.000Z" });

export const REQUEST_LIST: RequestList = { schema_version: 1, open: 2, requests: REQUESTS, claim: CLAIM };

/** What each answered request was answered with (`requests.md`, "The request record"). */
const BASE_CASES: Case[] = [
  make(R2, R2_BLOCK, { stop_text: "w1's second try fails on the same test. Do you want it split into two units?" }, {
    verb: "answer",
    text: "Yes, split it.",
    option: { id: "s1", by: "seat", recommended: false },
  }),
  make(R3, R3_BLOCK, { gaps: [{ section: "AC-3", question: "May the page style files name a token only?", severity: "blocking" }] }, {
    verb: "answer",
    text: "Yes: tokens only.",
  }),
  make(R4, R4_BLOCK, { head_branch: "spine/r1.s4", base: "main" }, { verb: "ack", head_sha: "1d".repeat(20) }),
  make(R10, R10_BLOCK, {}, { verb: "answer", text: "restarted the database on draco-desk", to_stage: "implement" }),
  make(
    OPEN_STOP,
    R5_BLOCK,
    {
      stop_text: `AC-8's command exits 0 at the base only because the script does not exist yet. May the RED gate skip it?\n\nOptions:\n- ${STOP_OPTIONS[0]}\n- [recommended] ${STOP_OPTIONS[1]}`,
    },
  ),
  make(OPEN_ACK, R6_BLOCK, { head_branch: "spine/r1.s5", base: "main", head_sha: HEAD_SHA, pr: PR_URL }),
];

// ---- the cases the base does not hold (frames 12a to 12c) ----

const LONG_OPTIONS = [
  "Keep the old sessions columns for one more release and write to both, so the Android app built for Release 1 keeps reading them until it updates; drop them in r3.",
  "Drop the old sessions columns now and add a read shim in the server that maps them from the new table, so old clients keep working without a second write path to maintain.",
  "Drop the old columns now with no shim; the Android app is updated in the same release and nothing else reads them, so the extra code would never run in practice.",
  "Stop here and ask the operator in the chat which clients still read the old columns before deciding, because the answer changes how much of this unit is left to build.",
];

const STALL = block({
  cause: "stalled",
  summary: "The seat of unit w5 stalled at stage build (attempt 2) after its one relaunch.",
  options: [resumeOption("resume", "Resume", "Resumes the unit at its stage.")],
  recommended: { option: "resume", by: "controller", why: "A stall is often transient: a new launch may go on." },
  refs: { project: PROJECT, run: RUN, unit: "w5", stage: "build" },
  attempt: 2,
  about: { role: "implementer", unit: "w5" },
});

const CAP_OPTIONS = [
  resumeOption("again", "Send it to fix again", "Resumes the unit at fix; the outcome counters start again.", "fix"),
  resumeOption("go_on", "Go on here", "Resumes the unit at review; the outcome counters start again."),
];
const CAP = block({
  cause: "outcome_cap",
  summary: "Stage review of unit w4 sent the unit to fix 3 times, which is its cap, so the unit parked.",
  options: CAP_OPTIONS,
  refs: { project: PROJECT, run: RUN, unit: "w4", stage: "review" },
  attempt: 3,
});

const GATE = block({
  cause: "park_file",
  summary: "Unit s1 parked at stage close because the seat wrote a park file.",
  options: [
    answerOption("w1", "Let the Correction seat add the down step", "workflow", { effect: { verb: "answer", text: "Add the down step in the Correction seat.", to: "correction" } }),
    answerOption("w2", "Hold the run so I can fix it myself", "workflow", { effect: { verb: "answer", text: "I will fix the migration myself; hold the run." } }),
  ],
  recommended: { option: "w1", by: "workflow", why: "The workflow marked this option." },
  refs: { project: PROJECT, run: RUN, unit: "s1", stage: "close" },
  asked_by: seat("spine_close", "s1"),
});

const GAPS = block({
  cause: "park_file",
  summary: "Unit s2 parked at stage plan because the seat wrote a park file.",
  refs: { project: PROJECT, run: RUN, unit: "s2", stage: "plan" },
  asked_by: seat("round_planner", "s2"),
});

const PARK_DECLARED = block({
  cause: "park_file",
  summary: "Unit w1 parked at stage review because the seat wrote a park file.",
  options: [
    answerOption("w1", "The schema is approved: go on", "workflow", { effect: { verb: "answer", text: "The schema is approved: go on.", to: "build" } }),
    answerOption("w2", "Hold W-1: I will change the schema first", "workflow", { effect: { verb: "answer", text: "Hold: I will change the schema first." } }),
  ],
  refs: { project: PROJECT, run: RUN, unit: "w1", stage: "review" },
  asked_by: seat("reviewer", "w1"),
});

const PARK_NOTHING = block({
  cause: "park_file",
  summary: "Unit w3 parked at stage review because the seat wrote a park file.",
  options: [
    resumeOption("resume", "Resume", "Resumes the unit at its stage. Remove the park file first, or the unit parks again.", undefined),
  ],
  refs: { project: PROJECT, run: RUN, unit: "w3", stage: "review" },
  asked_by: seat("reviewer", "w3"),
  takes_text: false,
});

const NO_DONE = block({
  cause: "no_done_signal",
  summary: "The seat of unit w2 ended its turn at stage build with no done signal (attempt 1).",
  options: [resumeOption("resume", "Resume", "Resumes the unit at its stage; the stage launches again.")],
  refs: { project: PROJECT, run: RUN, unit: "w2", stage: "build" },
  about: { role: "implementer", unit: "w2" },
});

const MERGE_FAILED = block({
  cause: "merge_checks_red",
  summary: "The checks of the PR pulseai-labs/pulse-factory#80 of unit r1.s5 are red.",
  options: [resumeOption("resume", "Resume", "Tries the merge again.")],
  recommended: { option: "resume", by: "controller", why: "A red check may be a flaky run: the merge is tried again." },
  refs: { project: PROJECT, run: RUN, stage: "merge", pr: PR },
  attempt: 0,
  takes_text: false,
});

const CAP_NO_TEXT = { ...CAP, takes_text: false };

const STOP_NO_TEXT = block({
  cause: "stop_file",
  summary: "Unit w2 stopped at stage review and wrote a stop file.",
  refs: { project: PROJECT, run: RUN, unit: "w2", stage: "review" },
  asked_by: seat("reviewer", "w2"),
  takes_text: false,
});

const T = (n: number) => `2026-11-08T${n < 60 ? "14" : "15"}:${String(n % 60).padStart(2, "0")}:00.000Z`;
const QUESTION = "The spec says coupons apply before discounts, but the loyalty discount is applied at checkout, not in pricing. Should a coupon stack with the loyalty discount, or replace it?";

const run1 = (kind: string, patch: Partial<RequestSummary>): Partial<RequestSummary> => ({ kind, ...patch });

export const CASES: Record<string, Case> = Object.fromEntries(
  [
    // 12a: the stall, the outcome cap, the red gate and the gaps request
    make(row("R-21", STALL, run1("stall", { unit: "w5", stage: "build", opened_at: T(40), reason: "the seat stalled" })), STALL, {}),
    make(row("R-20", CAP, run1("park", { unit: "w4", stage: "review", opened_at: T(25), reason: "the fix cap of 3 was reached" })), CAP, {}),
    make(row("R-18", GATE, run1("red_gate", { unit: "s1", stage: "close", opened_at: T(10), reason: "migrations/down: 0007_sessions_table has no down step; rollback test failed (exit 1); see runs/s1/gate.log" })), GATE, {}),
    make(
      row("R-15", GAPS, run1("gaps", { unit: "s2", stage: "plan", opened_at: T(5), reason: "the planner asks about gaps in the plan" })),
      GAPS,
      {
        gaps: [
          { section: "AC-2", question: "Which table owns agent definitions: sessions, or a new agents table?", severity: "blocking" },
          { section: "AC-4", question: "Should the migration keep the old columns for one release?", severity: "blocking" },
        ],
      },
    ),
    // 12b: the question states
    make(row("R-31", { ...R5_BLOCK, options: [], recommended: undefined, summary: "Unit w2 stopped at stage implement and wrote a stop file." }, { unit: "w2", stage: "implement", opened_at: T(1), reason: "the implementer asks a question" }), { ...R5_BLOCK, options: [], recommended: undefined }, { stop_text: QUESTION }),
    make(
      row("R-32", R5_BLOCK, { unit: "w2", stage: "implement", opened_at: T(2), reason: "the implementer asks a question" }),
      {
        ...R5_BLOCK,
        recommended: undefined,
        options: [answerOption("s1", "Stack them: apply the coupon first, then the loyalty discount.", "seat"), answerOption("s2", "Replace it: a coupon turns off the loyalty discount for that order.", "seat")],
      },
      { stop_text: `${QUESTION}\n\nOptions:\n- Stack them: apply the coupon first, then the loyalty discount.\n- Replace it: a coupon turns off the loyalty discount for that order.` },
    ),
    make(
      row("R-33", R5_BLOCK, { unit: "w5", stage: "build", opened_at: T(3), reason: "the implementer asks a question" }),
      {
        ...R5_BLOCK,
        summary: "Unit w5 stopped at stage build and wrote a stop file.",
        attempt: 2,
        refs: { project: PROJECT, run: RUN, unit: "w5", stage: "build" },
        asked_by: seat("implementer", "w5"),
        options: LONG_OPTIONS.map((label, i) => answerOption(`s${i + 1}`, label, "seat")),
        recommended: { option: "s2", by: "seat", why: "The seat marked this option." },
      },
      {
        stop_text: `Two clients read sessions.agent and sessions.model today. The new agents table replaces them. What happens to the old columns?\n\nOptions:\n${LONG_OPTIONS.map((o, i) => `- ${i === 1 ? "[recommended] " : ""}${o}`).join("\n")}`,
      },
    ),
    make(
      row("R-34", R5_BLOCK, { unit: "w6", stage: "build", opened_at: T(4), reason: "the implementer asks a question" }),
      {
        ...R5_BLOCK,
        summary: "Unit w6 stopped at stage build and wrote a stop file.",
        refs: { project: PROJECT, run: RUN, unit: "w6", stage: "build" },
        asked_by: seat("implementer", "w6"),
        options: [],
        recommended: undefined,
        options_error: "an option is over 200 characters",
      },
      { stop_text: "Should the cache key include the locale?\n\nOptions:\n- Yes\n- No, and drop the cache" },
    ),
    // 12b: a request with no block (opened before the upgrade)
    make(requestSummary("R-35", { run: RUN, unit: "w7", stage: "build", opened_at: T(6), reason: "the implementer asks a question" }), undefined, { stop_text: "May the migration run before the deploy?" }),
    // 12c: the other cards
    make(row("R-22", PARK_DECLARED, run1("park", { unit: "w1", stage: "review", opened_at: T(20), reason: "the reviewer parked and asks for a decision" })), PARK_DECLARED, {}),
    make(row("R-23", PARK_NOTHING, run1("park", { unit: "w3", stage: "review", opened_at: T(30), reason: "the reviewer parked and asks for a decision" })), PARK_NOTHING, {}),
    make(row("R-24", NO_DONE, run1("park", { unit: "w2", stage: "build", opened_at: T(35), reason: "the seat ended its turn with no done signal" })), NO_DONE, {}),
    make(row("R-25", MERGE_FAILED, run1("merge_failed", { unit: null, stage: "merge", opened_at: T(45), reason: "the checks are red", pr: PR_URL, base: "main", head_sha: HEAD_SHA })), MERGE_FAILED, {}),
    make(row("R-26", CAP_NO_TEXT, run1("park", { unit: "w4", stage: "review", opened_at: T(25), reason: "the fix cap of 3 was reached" })), CAP_NO_TEXT, {}),
    make(row("R-27", STOP_NO_TEXT, run1("stop", { unit: "w2", stage: "review", opened_at: T(28), reason: "the reviewer asks a question" })), STOP_NO_TEXT, { stop_text: "Is the review done?" }),
  ].map((c): [string, Case] => [c.request.id, c]),
);

/** The cases of a screen, in the order the page shows them: the page sorts, so the order here is for reading. */
export const pick = (...ids: string[]): Case[] => ids.map((id) => CASES[id] as Case);

/** The request IDs of the state screens, by screen. */
export const SCREENS = {
  page: ["R-15", "R-18", "R-20", "R-21"],
  questionStates: ["R-31", "R-32", "R-33", "R-34", "R-35"],
  otherCards: ["R-22", "R-23", "R-24", "R-25", "R-26", "R-27"],
} as const;

/** What `show` answers, by request ID: the base's and the cases'. */
export const DETAILS: Record<string, RequestDetail> = Object.fromEntries([...BASE_CASES, ...Object.values(CASES)].map((c): [string, RequestDetail] => [c.request.id, c.detail]));

/** The request frame that opens every stream: the snapshot of the requests and the claim. */
export const REQUESTS_FRAME: RequestFrame = requestFrames.requests(REQUESTS, CLAIM);

/** A request list: the base's answered rows and claim with `open` as the only open ones (or the base's). */
export function listOf(open: Case[], keepBaseOpen: boolean): RequestList {
  const base = keepBaseOpen ? REQUESTS : REQUESTS.filter((r) => r.status === "answered");
  const openRows = open.map((c) => c.request);
  const baseOpen = keepBaseOpen ? 2 : 0;
  return { schema_version: 1, open: baseOpen + openRows.length, requests: [...base, ...openRows], claim: CLAIM };
}

/** The notice of the opened R-5, as the controller sends it: the text is the controller's, cut to the word cap. */
export const NOTICE: NoticeRecord = {
  position: 3,
  event: "opened",
  request: "R-5",
  revision: 1,
  run: RUN,
  project: PROJECT,
  unit: "w2",
  kind: "stop",
  cause: "stop_file",
  text: "Unit w2 stopped at stage implement and wrote a stop file.",
  created_at: "2026-11-08T13:58:00.000Z",
  sends: 1,
};

export const verbs: Verbs = {
  requests: () => REQUEST_LIST,
  show: (args) => {
    const id = typeof args === "object" && args !== null ? (args as { id?: unknown }).id : undefined;
    const detail = typeof id === "string" ? DETAILS[id] : undefined;
    if (detail === undefined) throw new Error(`show: no fixture for request ${String(id)}`);
    return detail;
  },
  // The code is test data and never shown: the app keeps it in memory for one press.
  confirm_code: (args) => {
    const subject = (args as { subject?: { request?: string } }).subject;
    const detail = typeof subject?.request === "string" ? DETAILS[subject.request] : undefined;
    if (detail === undefined) throw new Error(`confirm_code: no fixture for request ${String(subject?.request)}`);
    return {
      schema_version: 1,
      replayed: false,
      code: `pfc1-${"0".repeat(31)}1`,
      expires_at: "2026-11-08T14:57:00.000Z",
      device: "D-1",
      generation: CLAIM.generation,
      holder: CLAIM,
      subject: { request: detail.id, revision: detail.revision, kind: detail.kind, run: detail.run, unit: detail.unit, reason: detail.reason },
    };
  },
  notice_ack: (args) => {
    const a = args as { position: number; outcome: string };
    return { schema_version: 1, device: "D-1", position: a.position, outcome: a.outcome, admitted: true };
  },
};
