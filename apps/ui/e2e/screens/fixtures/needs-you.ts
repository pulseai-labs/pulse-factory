// The d25 test data of the Needs you page and the dock: the requests, the claim and the details that `show`
// answers. Owner: w4. The shell's counts and the stream's request frame read these too. The parked item
// (`PARKED`) is not in the base list: one more open request would change the rail's count on every other
// owner's screenshot, so the `3f-needs-you` test adds it through `overlay` (`screens.spec.ts`).
import type { RequestDetail, RequestFrame, RequestList, RequestSummary } from "@pulse-factory/api-client";
import { claimRecord, mergeAck, requestFrames, requestSummary } from "../../../src/test/fake-transport";
import type { Verbs } from "./verbs";

const HEAD_SHA = "9c1e7a4b20d5f86e3a71c0b4d2e95f18a6b3c7d0";
const PR_URL = "https://github.com/pulseai-labs/pulse-factory/pull/80";

const OPEN_STOP: RequestSummary = requestSummary("R-5", {
  run: "r0-s19-1",
  unit: "w2",
  stage: "implement",
  opened_at: "2026-11-08T13:58:00.000Z",
  reason: "the implementer asks a question",
  blocks: ["w2"],
});
const OPEN_ACK: RequestSummary = mergeAck("R-6", {
  run: "r0-s19-1",
  opened_at: "2026-11-08T14:20:00.000Z",
  pr: PR_URL,
  base: "main",
  head_sha: HEAD_SHA,
  blocks: ["w4", "w5"],
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

/** A CLI claim: the answered rows of the generation it holds read "CLI · claim session C-2". */
const CLAIM = claimRecord({ generation: 3, holder: "session C-2", device: null, claimed_at: "2026-11-08T09:30:00.000Z" });

/** A parked item (R-7): the unit's third try failed. It takes `answer` and `clear`; the card offers Resume. */
export const PARKED: RequestSummary = requestSummary("R-7", {
  kind: "park",
  run: "r0-s19-1",
  unit: "w3",
  stage: "implement",
  opened_at: "2026-11-08T14:05:00.000Z",
  reason: "the build failed three times: cargo test exits 101",
  blocks: ["w3"],
});


export const REQUEST_LIST: RequestList = { schema_version: 1, open: 2, requests: REQUESTS, claim: CLAIM };

/** The body `show` answers for each request: the question of a stop or a gaps request, the branch of a merge. */
const BODIES: Record<string, Record<string, unknown>> = {
  "R-2": { stop_text: "w1's second try fails on the same test. Do you want it split into two units?" },
  "R-3": { gaps: [{ section: "AC-3", question: "May the page style files name a token only?", severity: "blocking" }] },
  "R-4": { head_branch: "spine/r1.s4", base: "main" },
  "R-5": {
    stop_text: "AC-8's command exits 0 at the base only because the script does not exist yet. May the RED gate skip it?",
    gaps: [{ section: "AC-8", question: "Is a mutated-bundle run that fails with rc 127 the expected mutation failure?", severity: "blocking" }],
  },
  "R-6": { head_branch: "spine/r1.s5", base: "main", head_sha: HEAD_SHA, pr: PR_URL },
  "R-7": {},
};

/** What each answered request was answered with. */
const ANSWERS: Record<string, string> = {
  "R-2": "Yes, split it.",
  "R-3": "Yes: tokens only.",
};

/** What `show` answers, by request ID. */
export const DETAILS: Record<string, RequestDetail> = Object.fromEntries(
  [...REQUESTS, PARKED].map((r): [string, RequestDetail] => [
    r.id,
    {
      schema_version: 1,
      body: BODIES[r.id] ?? {},
      answer: r.status === "answered" ? { text: ANSWERS[r.id] ?? "Yes." } : null,
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

/** The request frame that opens every stream: the snapshot of the requests and the claim. */
export const REQUESTS_FRAME: RequestFrame = requestFrames.requests(REQUESTS, CLAIM);

export const verbs: Verbs = {
  requests: () => REQUEST_LIST,
  show: (args) => {
    const id = typeof args === "object" && args !== null ? (args as { id?: unknown }).id : undefined;
    const detail = typeof id === "string" ? DETAILS[id] : undefined;
    if (detail === undefined) throw new Error(`show: no fixture for request ${String(id)}`);
    return detail;
  },
};
