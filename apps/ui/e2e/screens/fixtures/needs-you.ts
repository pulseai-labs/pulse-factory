// The d25 test data of the Needs you page and the dock: the requests, the claim and the details that `show`
// answers. Owner: w4. The shell's counts and the stream's request frame read these too.
import type { RequestDetail, RequestFrame, RequestList, RequestSummary } from "@pulse-factory/api-client";
import { claimRecord, mergeAck, requestFrames, requestSummary } from "../../../src/test/fake-transport";
import type { Verbs } from "./verbs";

const HEAD_SHA = "9c1e7a4b20d5f86e3a71c0b4d2e95f18a6b3c7d0";

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
