// What the page and the dock share (r1.s5.w4): the open requests, the bodies they need, the one-press acts and
// Hold. The page adds its reads (`requests`, `runList`) and Answered today on top; the dock adds nothing: its
// requests come from the stream's frames with no call, and its `show` calls fill the bodies the cards read.
import type { RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import { useCallback, useMemo } from "react";
import { answeredRecently, useRequests, type RequestsState } from "../state/requests";
import type { CardEnv } from "./cards";
import { useHeld } from "./held";
import { useConfirmedAct, type ConfirmedActs } from "./useConfirmedAct";
import { useRequestBodies, type RequestBodies } from "./useRequestBodies";

export interface NeedsYou {
  state: RequestsState;
  open: RequestSummary[];
  /** The requests answered or withdrawn in the last day, newest first. */
  answered: RequestSummary[];
  held: ReadonlyMap<string, number>;
  acts: ConfirmedActs;
  bodies: RequestBodies;
  /** The spine's name of a run from the last run list, or `null`. */
  spineOf(run: string): string | null;
  /** A body held for the request at any revision, the newest first: the question of an answered row. */
  bodyFor(id: string): RequestDetail | undefined;
  envOf(request: RequestSummary, compact: boolean): CardEnv;
}

export function useNeedsYou(): NeedsYou {
  const { state, open } = useRequests();
  const acts = useConfirmedAct();
  const held = useHeld();
  const answered = useMemo(() => answeredRecently(state, Date.now()), [state]);
  // Every open request needs its body (the merge card's PR, From and head SHA come from it, a run question's
  // text too), and every answered row this page did not answer itself needs its detail (the answer, who).
  const want = useMemo(() => [...open, ...answered.filter((r) => acts.done[r.id] === undefined)], [open, answered, acts.done]);
  const bodies = useRequestBodies(want);
  const { detailOf, errors, retry } = bodies;

  const spineOf = useCallback(
    (run: string) => state.runs?.runs.find((r) => r.id === run)?.spine ?? null,
    [state.runs],
  );

  const bodyFor = useCallback(
    (id: string) => {
      let best: RequestDetail | undefined;
      for (const detail of Object.values(state.bodies)) {
        if (detail.id === id && (best === undefined || detail.revision > best.revision)) best = detail;
      }
      return best;
    },
    [state.bodies],
  );

  const envOf = useCallback(
    (request: RequestSummary, compact: boolean): CardEnv => ({
      detail: detailOf(request),
      bodyError: errors[request.id] ?? null,
      spine: spineOf(request.run),
      line: acts.lines[request.id],
      done: acts.done[request.id],
      notice: acts.notices[request.id],
      inFlight: acts.inFlight(request.id),
      compact,
      onPress: (kind, text) => acts.press(request, kind, text),
      onRetry: () => acts.retry(request.id),
      onRefresh: retry,
    }),
    [detailOf, errors, spineOf, acts, retry],
  );

  return { state, open, answered, held, acts, bodies, spineOf, bodyFor, envOf };
}
