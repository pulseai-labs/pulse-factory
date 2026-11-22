// The told signal in the UI (r2.s11.w5, Approach 9; 12f; Q9). A `NoticesProvider` sits inside the shell: it listens for
// the notice frames of the one stream (`chat.onNotice`), gathers them into a batch and, when the batch ends, shows
// one notification for its `opened` notices and acknowledges the batch once.
//
// The stream gives no end mark for the notices a reconnect sends, so a notice frame starts a batch, and the batch
// ends 300 ms after its last frame and at most 1,000 ms after its first. The one timer starts only on a frame,
// fires once and stops itself: while no frame arrives the app sets no timer and makes no call. The app calls
// `noticeAck` only after a notice frame, so it never acknowledges a server that sent none. A failed `noticeAck`
// is not tried again by the app: the server sends the notice again, and the next batch acknowledges it without a
// second notification. Nothing of a notice goes to browser storage, the console or a URL; the blocked state lives in
// memory only.
import type { NoticeRecord } from "@pulse-factory/api-client";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { Notifier, NotifyOutcome } from "../notify/notifier";
import { useNavigation } from "../shell/navigation";
import { useChat } from "./chat";
import { useConnection } from "./connection";
import { useRequests } from "./requests";

/** A batch ends this long after its last frame, and at most `BATCH_MAX_MS` after its first. */
export const BATCH_QUIET_MS = 300;
export const BATCH_MAX_MS = 1_000;

const WHAT: Record<string, (unit: string) => string> = {
  stop: (u) => `${u} asks a question`,
  gaps: (u) => `${u} found gaps`,
  red_gate: (u) => `${u}’s gate failed`,
  park: (u) => `${u} is parked`,
  stall: (u) => `${u} stalled`,
  seat_lost: (u) => `${u} lost its seat`,
  merge_failed: (u) => `${u}’s merge failed`,
  merge_ack: (u) => `${u} is ready to merge`,
};

/** `<what happened>` of a notice, by its kind, with the stored unit or else the run ID. */
export function whatHappened(notice: NoticeRecord): string {
  const make = WHAT[notice.kind];
  return make === undefined ? "a request opened" : make(notice.unit ?? notice.run);
}

export interface Told {
  /** The last `noticeAck` this app sent carried `blocked`. */
  blocked: boolean;
  /** A request that a click named: the page scrolls to its card once, then clears it. */
  scrollTo: string | null;
  clearScroll(): void;
}

const Ctx = createContext<Told | null>(null);

export function useTold(): Told {
  const c = useContext(Ctx);
  if (!c) throw new Error("useTold outside a NoticesProvider");
  return c;
}

export function NoticesProvider(props: { notifier: Notifier; children: ReactNode }) {
  const { notifier, children } = props;
  const { call, projects } = useConnection();
  const { onNotice } = useChat();
  const { state } = useRequests();
  const { openPage } = useNavigation();
  const [blocked, setBlocked] = useState(false);
  const [scrollTo, setScrollTo] = useState<string | null>(null);

  // What a batch reads when it ends: the latest of each, with no re-render of the provider.
  const latest = useRef({ projects, byId: state.byId });
  latest.current = { projects, byId: state.byId };
  const batch = useRef<NoticeRecord[]>([]);
  const firstAt = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** The highest position this app has acknowledged on this server. */
  const acked = useRef(0);
  /** The positions of the `opened` notices this app has shown in this run. */
  const shown = useRef(new Set<number>());
  const chain = useRef<Promise<void>>(Promise.resolve());
  const live = useRef(true);

  const projectName = useCallback((n: NoticeRecord): string => {
    if (n.project === undefined) return n.run;
    return latest.current.projects?.find((p) => p.id === n.project)?.name ?? n.project;
  }, []);

  const process = useCallback(
    async (frames: NoticeRecord[]) => {
      const fresh = frames.filter((n) => n.position > acked.current);
      if (fresh.length === 0) return;
      const top = Math.max(...fresh.map((n) => n.position));
      const closed = new Set(fresh.filter((n) => n.event === "closed").map((n) => n.request));
      const seen = new Set<number>();
      const alerts = fresh
        .filter((n) => {
          if (n.event !== "opened" || shown.current.has(n.position) || seen.has(n.position)) return false;
          seen.add(n.position);
          if (closed.has(n.request)) return false;
          const held = latest.current.byId[n.request];
          return held === undefined || held.status === "open";
        })
        .sort((a, b) => a.position - b.position);
      let outcome: NotifyOutcome = "shown";
      if (alerts.length > 0) {
        const newest = alerts[alerts.length - 1] as NoticeRecord;
        const note =
          alerts.length === 1
            ? { title: `${projectName(newest)} · ${whatHappened(newest)}`, body: `${newest.text} ${newest.request}`, request: newest.request }
            : {
                title: `Pulse Factory · ${alerts.length} new requests`,
                body: `Newest: ${projectName(newest)} · ${newest.text} ${newest.request}`,
                request: newest.request,
              };
        try {
          outcome = await notifier.show(note);
        } catch {
          outcome = "blocked";
        }
        if (outcome === "shown") for (const n of alerts) shown.current.add(n.position);
      }
      if (live.current) setBlocked(outcome === "blocked");
      try {
        const answer = await call((t) => t.noticeAck({ position: top, outcome }));
        acked.current = Math.max(acked.current, answer.position, top);
      } catch {
        // Not tried again here: the server sends the notice again and the next batch acknowledges it.
      }
    },
    [call, notifier, projectName],
  );

  const flush = useCallback(() => {
    timer.current = null;
    const frames = batch.current;
    batch.current = [];
    chain.current = chain.current.then(() => process(frames)).catch(() => undefined);
  }, [process]);

  useEffect(() => {
    live.current = true;
    const stop = onNotice((frame) => {
      const now = Date.now();
      if (batch.current.length === 0) firstAt.current = now;
      batch.current.push(frame.notice);
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(flush, Math.max(0, Math.min(BATCH_QUIET_MS, firstAt.current + BATCH_MAX_MS - now)));
    });
    return () => {
      live.current = false;
      stop();
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = null;
    };
  }, [onNotice, flush]);

  // A click opens the Needs you page and names the request whose card the page scrolls to.
  useEffect(
    () =>
      notifier.onOpen((request) => {
        openPage("needs_you");
        setScrollTo(request);
      }),
    [notifier, openPage],
  );

  const clearScroll = useCallback(() => setScrollTo(null), []);
  const value = useMemo<Told>(() => ({ blocked, scrollTo, clearScroll }), [blocked, scrollTo, clearScroll]);
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
