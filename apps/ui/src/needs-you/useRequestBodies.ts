// The bodies of the requests a Needs-you surface shows (r1.s5.w4, Approach 4). One `show` for each request
// it is asked to hold a body for, at the revision the request has now: at open, and when a frame brings a new
// revision. A body is kept by `<id>@<revision>` in the shared requests state, so the page and the dock never
// read the same one twice. A failed `show` is tried again only on `retry` (the page's Refresh). No timer, no
// interval and no stream of its own.
import type { RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { bodyKey, useRequests } from "../state/requests";

export interface RequestBodies {
  /** The body of a request at its current revision, once `show` answered. */
  detailOf(request: RequestSummary): RequestDetail | undefined;
  /** The line of a failed `show`, by request ID. */
  errors: Record<string, string>;
  /** Try the failed `show` calls again. */
  retry(): void;
}

export function useRequestBodies(want: RequestSummary[]): RequestBodies {
  const { call } = useConnection();
  const { state, apply } = useRequests();
  const [errors, setErrors] = useState<Record<string, string>>({});
  // A bump asks the effect to try again after a retry.
  const [tick, setTick] = useState(0);
  const requested = useRef(new Set<string>());
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const load = useCallback(
    async (request: RequestSummary) => {
      const key = bodyKey(request.id, request.revision);
      if (requested.current.has(key)) return;
      requested.current.add(key);
      try {
        const detail = await call((t) => t.show({ id: request.id }));
        // The detail applies to the shared state even when this surface is gone: the body is the server's.
        apply({ type: "body", detail });
        if (!live.current) return;
        setErrors((m) => {
          if (!(request.id in m)) return m;
          const rest = { ...m };
          delete rest[request.id];
          return rest;
        });
      } catch (e) {
        // The key stays in `requested`: only `retry` (the page's Refresh) asks again, so a failure never loops.
        const err = asTransportError(e);
        if (live.current && !isDownKind(err.kind)) setErrors((m) => ({ ...m, [request.id]: err.message }));
      }
    },
    [call, apply],
  );

  useEffect(() => {
    for (const r of want) {
      if (!state.bodies[bodyKey(r.id, r.revision)]) void load(r);
    }
  }, [want, state.bodies, load, tick]);

  const retry = useCallback(() => {
    for (const key of [...requested.current]) {
      if (!state.bodies[key]) requested.current.delete(key);
    }
    setErrors({});
    setTick((n) => n + 1);
  }, [state.bodies]);

  const detailOf = useCallback(
    (request: RequestSummary) => state.bodies[bodyKey(request.id, request.revision)],
    [state.bodies],
  );

  return { detailOf, errors, retry };
}
