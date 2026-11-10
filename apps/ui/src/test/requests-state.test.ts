// AC-9 (r1.s4.w3): the rules of the requests state, on fixed sequences of frames, reads and acts.
import type { RequestActed, RequestSummary } from "@pulse-factory/api-client";
import { describe, expect, it } from "vitest";
import {
  INITIAL_REQUESTS,
  answeredRecently,
  bodyKey,
  openRequests,
  reduceRequests,
  type RequestsAction,
  type RequestsState,
} from "../state/requests";
import { claimRecord, mergeAck, requestFrames, requestSummary, runSummary } from "./fake-transport";

function run(actions: RequestsAction[], from: RequestsState = INITIAL_REQUESTS): RequestsState {
  return actions.reduce(reduceRequests, from);
}

const frame = (f: ReturnType<(typeof requestFrames)[keyof typeof requestFrames]>): RequestsAction => ({
  type: "frame",
  frame: f,
});

function acted(patch: Partial<RequestActed> = {}): RequestActed {
  return {
    schema_version: 1,
    replayed: false,
    verb: "answer",
    device: "D-1",
    generation: 2,
    request: null,
    merged: null,
    head_sha: null,
    pr: null,
    base: null,
    outcome: null,
    resumed: null,
    hook: null,
    to_stage: null,
    ...patch,
  };
}

const closed = (id: string, patch: Partial<RequestSummary> = {}) =>
  requestSummary(id, { status: "answered", answered_at: "2026-11-10T11:00:00.000Z", verbs: [], ...patch });

describe("the requests state: one request", () => {
  it("keeps a request by ID and ignores a frame at a lower revision", () => {
    const state = run([
      frame(requestFrames.request(requestSummary("R-1", { revision: 3, reason: "third" }))),
      frame(requestFrames.request(requestSummary("R-1", { revision: 2, reason: "second" }))),
    ]);
    expect(Object.keys(state.byId)).toEqual(["R-1"]);
    expect(state.byId["R-1"]?.revision).toBe(3);
    expect(state.byId["R-1"]?.reason).toBe("third");
  });

  it("takes a higher revision over the held one", () => {
    const state = run([
      frame(requestFrames.request(requestSummary("R-1", { revision: 1, reason: "first" }))),
      frame(requestFrames.request(requestSummary("R-1", { revision: 2, reason: "second" }))),
    ]);
    expect(state.byId["R-1"]?.reason).toBe("second");
  });

  it("lets a status other than open win over open at the same revision, and never the other way", () => {
    const open = requestSummary("R-1");
    const done = closed("R-1");
    const forward = run([frame(requestFrames.request(open)), frame(requestFrames.request(done))]);
    expect(forward.byId["R-1"]?.status).toBe("answered");
    const backward = run([frame(requestFrames.request(done)), frame(requestFrames.request(open))]);
    expect(backward.byId["R-1"]?.status).toBe("answered");
    expect(openRequests(backward)).toEqual([]);
  });

  it("keeps the same state object for a duplicate frame", () => {
    const once = run([frame(requestFrames.request(requestSummary("R-1")))]);
    const twice = reduceRequests(once, frame(requestFrames.request(requestSummary("R-1"))));
    expect(twice).toBe(once);
    const lower = reduceRequests(
      run([frame(requestFrames.request(requestSummary("R-1", { revision: 2 })))]),
      frame(requestFrames.request(requestSummary("R-1", { revision: 1 }))),
    );
    expect(lower.byId["R-1"]?.revision).toBe(2);
  });
});

describe("the requests state: snapshots and reads", () => {
  it("replaces the open requests with a snapshot: a held open request it does not list is dropped", () => {
    const state = run([
      frame(requestFrames.request(requestSummary("R-1"))),
      frame(requestFrames.request(requestSummary("R-2"))),
      frame(requestFrames.requests([requestSummary("R-2")])),
    ]);
    expect(Object.keys(state.byId)).toEqual(["R-2"]);
    expect(state.loaded).toBe(true);
  });

  it("does the same for a `requests` read", () => {
    const state = run([
      frame(requestFrames.request(requestSummary("R-1"))),
      {
        type: "read",
        list: { schema_version: 1, open: 1, requests: [requestSummary("R-2")], claim: null },
      },
    ]);
    expect(Object.keys(state.byId)).toEqual(["R-2"]);
  });

  it("keeps a closed request that the snapshot does not list, and applies one it lists", () => {
    const state = run([
      frame(requestFrames.request(closed("R-1"))),
      frame(requestFrames.requests([closed("R-3"), requestSummary("R-2")])),
    ]);
    expect(Object.keys(state.byId).sort()).toEqual(["R-1", "R-2", "R-3"]);
    expect(openRequests(state).map((r) => r.id)).toEqual(["R-2"]);
  });

  it("marks the state loaded with the first snapshot even when it is empty", () => {
    expect(INITIAL_REQUESTS.loaded).toBe(false);
    expect(run([frame(requestFrames.requests([]))]).loaded).toBe(true);
  });

  it("returns the same state for a snapshot that changes nothing", () => {
    const once = run([frame(requestFrames.requests([requestSummary("R-1")], claimRecord()))]);
    const again = reduceRequests(once, frame(requestFrames.requests([requestSummary("R-1")], claimRecord())));
    expect(again).toBe(once);
  });
});

describe("the requests state: the claim", () => {
  it("takes a claim by generation: a lower one is ignored, a higher one applies", () => {
    const state = run([
      frame(requestFrames.claim(claimRecord({ generation: 3, holder: "three" }))),
      frame(requestFrames.claim(claimRecord({ generation: 2, holder: "two" }))),
    ]);
    expect(state.claim?.holder).toBe("three");
    const next = reduceRequests(state, frame(requestFrames.claim(claimRecord({ generation: 4, holder: "four" }))));
    expect(next.claim?.holder).toBe("four");
  });

  it("applies the same generation, so a revoke of the holder shows", () => {
    const state = run([
      frame(requestFrames.claim(claimRecord({ generation: 3 }))),
      frame(requestFrames.claim(claimRecord({ generation: 3, revoked: true }))),
    ]);
    expect(state.claim?.revoked).toBe(true);
  });

  it("clears the claim when the server says there is none", () => {
    const state = run([frame(requestFrames.claim(claimRecord())), frame(requestFrames.claim(null))]);
    expect(state.claim).toBeNull();
  });

  it("reads the claim from a snapshot", () => {
    const state = run([frame(requestFrames.requests([], claimRecord({ holder: "ann@host" })))]);
    expect(state.claim?.holder).toBe("ann@host");
  });

  it("moves the claim to the device with an act, and the next claim frame replaces it", () => {
    const base = run([frame(requestFrames.claim(claimRecord({ generation: 1 })))]);
    const moved = reduceRequests(base, { type: "acted", acted: acted({ generation: 2, device: "D-1" }) });
    expect(moved.claim).toMatchObject({ generation: 2, device: "D-1", holder: "device D-1", revoked: false });
    const framed = reduceRequests(moved, frame(requestFrames.claim(claimRecord({ generation: 2, holder: "device D-1", device: "D-1" }))));
    expect(framed.claim?.generation).toBe(2);
  });

  it("does not move the claim for a replayed act or a lower generation", () => {
    const base = run([frame(requestFrames.claim(claimRecord({ generation: 5, holder: "ann@host" })))]);
    const replayed = reduceRequests(base, {
      type: "acted",
      acted: acted({ replayed: true, generation: null }),
    });
    expect(replayed.claim?.holder).toBe("ann@host");
    const lower = reduceRequests(base, { type: "acted", acted: acted({ generation: 4 }) });
    expect(lower.claim?.holder).toBe("ann@host");
  });

  it("applies the request an act answered like a frame", () => {
    const base = run([frame(requestFrames.request(requestSummary("R-1")))]);
    const state = reduceRequests(base, { type: "acted", acted: acted({ request: closed("R-1") }) });
    expect(state.byId["R-1"]?.status).toBe("answered");
    // A request at a lower revision does not undo it.
    const stale = reduceRequests(state, {
      type: "acted",
      acted: acted({ request: requestSummary("R-1", { revision: 1 }) }),
    });
    expect(stale.byId["R-1"]?.status).toBe("answered");
  });
});

describe("the requests state: bodies and runs", () => {
  it("keeps a body by ID and revision", () => {
    const detail = {
      schema_version: 1 as const,
      ...requestSummary("R-1", { revision: 2 }),
      body: { stop_text: "which one?" },
      answer: null,
      answer_generation: null,
    };
    const state = run([{ type: "body", detail }]);
    expect(state.bodies[bodyKey("R-1", 2)]?.body).toEqual({ stop_text: "which one?" });
    expect(state.bodies[bodyKey("R-1", 1)]).toBeUndefined();
  });

  it("keeps the last run list", () => {
    const runs = { schema_version: 1 as const, runs: [runSummary("R-1")] };
    expect(run([{ type: "runs", runs }]).runs).toBe(runs);
  });
});

describe("the open requests", () => {
  it("lists merge_ack first, then the rest by when they opened", () => {
    const state = run([
      frame(
        requestFrames.requests([
          requestSummary("R-3", { opened_at: "2026-11-10T10:03:00.000Z" }),
          requestSummary("R-1", { opened_at: "2026-11-10T10:01:00.000Z" }),
          mergeAck("R-9", { opened_at: "2026-11-10T10:09:00.000Z" }),
          requestSummary("R-2", { opened_at: "2026-11-10T10:02:00.000Z", kind: "gaps" }),
          closed("R-4"),
        ]),
      ),
    ]);
    expect(openRequests(state).map((r) => r.id)).toEqual(["R-9", "R-1", "R-2", "R-3"]);
  });

  it("breaks a tie by the number in the ID, not the text", () => {
    const at = "2026-11-10T10:00:00.000Z";
    const state = run([
      frame(requestFrames.requests([requestSummary("R-10", { opened_at: at }), requestSummary("R-9", { opened_at: at })])),
    ]);
    expect(openRequests(state).map((r) => r.id)).toEqual(["R-9", "R-10"]);
  });
});

describe("the requests answered in the last day", () => {
  const NOW = Date.parse("2026-11-11T12:00:00.000Z");
  const hoursAgo = (h: number) => new Date(NOW - h * 3_600_000).toISOString();

  it("keeps answered and withdrawn within 24 hours, newest first, and drops the rest", () => {
    const state = run([
      frame(
        requestFrames.requests([
          closed("R-1", { answered_at: hoursAgo(3) }),
          closed("R-2", { answered_at: hoursAgo(30) }),
          closed("R-3", { status: "withdrawn", answered_at: hoursAgo(1) }),
          closed("R-4", { status: "superseded", answered_at: hoursAgo(1) }),
          closed("R-5", { answered_at: null }),
          requestSummary("R-6"),
          closed("R-7", { answered_at: hoursAgo(23.5) }),
        ]),
      ),
    ]);
    expect(answeredRecently(state, NOW).map((r) => r.id)).toEqual(["R-3", "R-1", "R-7"]);
  });

  it("moves with the clock: a request that was recent a day later is not", () => {
    const state = run([frame(requestFrames.request(closed("R-1", { answered_at: hoursAgo(3) })))]);
    expect(answeredRecently(state, NOW)).toHaveLength(1);
    expect(answeredRecently(state, NOW + 22 * 3_600_000)).toHaveLength(0);
  });
});
