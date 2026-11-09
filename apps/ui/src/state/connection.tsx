// The connection to the paired server: every call the UI makes goes through `call`, which times it
// and moves the link between "up" and the four disconnected states (3c).
//
// When the UI calls (docs/contracts/ui-transport.md): on start, on opening a page, after an action
// and on "Retry now". The one timer that makes a call is the retry while the server is
// *unreachable* (no HTTP answer, so no request reaches the server). An HTTP refusal never starts a
// timer. While connected there is no timer, interval or focus handler that calls. A chat stream's
// `closed` reaches the same banner through `report`.
import type { ProjectRecord } from "@pulse-factory/api-client";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { TransportError, type PairedServer, type Transport, type TransportErrorKind } from "../transport/types";

export type DownKind = "unreachable" | "unauthorized" | "key_changed" | "rate_limited";

export type Link =
  | { state: "up" }
  | { state: "down"; kind: DownKind; message: string; attempt: number; nextAt: number | null };

/** The wait before each retry of `status()`: 4, 8, 16, 32 and then every 60 seconds. */
const RETRY_SECONDS = [4, 8, 16, 32, 60] as const;
export function retryDelaySeconds(attempt: number): number {
  return RETRY_SECONDS[Math.min(attempt, RETRY_SECONDS.length - 1)] ?? 60;
}

export function isDownKind(kind: TransportErrorKind): kind is DownKind {
  return kind === "unreachable" || kind === "unauthorized" || kind === "key_changed" || kind === "rate_limited";
}

export function asTransportError(e: unknown): TransportError {
  if (e instanceof TransportError) return e;
  return new TransportError("server", e instanceof Error ? e.message : "an unknown error");
}

export interface Connection {
  server: PairedServer;
  link: Link;
  /** The time of the last call in milliseconds, whether it worked or not. */
  lastCallMs: number | null;
  /** The wall time of the last answer from the server, or `null` before the first. */
  lastReachedAt: number | null;
  /** Rises when the link comes back, so each open page fetches once. */
  epoch: number;
  /** Ticks each second while the server is unreachable, so "Retrying in n s" counts down. */
  now: number;
  call<T>(fn: (t: Transport) => Promise<T>): Promise<T>;
  /**
   * Feeds a failure the UI saw outside `call`, such as a stream's `closed`, into the same 3c banner
   * and retry as a failed call. A kind that is no disconnect changes nothing.
   */
  report(err: TransportError): void;
  retryNow(): void;
  forget(): Promise<void>;
  /** Opens the pair screen (3a) again. It drops nothing: only a new pairing replaces the old one. */
  pairAgain(): void;
  projects: ProjectRecord[] | null;
  projectsAt: number | null;
  setProjects(projects: ProjectRecord[]): void;
}

const Ctx = createContext<Connection | null>(null);

export function useConnection(): Connection {
  const c = useContext(Ctx);
  if (!c) throw new Error("useConnection outside a ConnectionProvider");
  return c;
}

export function ConnectionProvider(props: {
  transport: Transport;
  server: PairedServer;
  onForgotten: (server: PairedServer) => void;
  onPairAgain: () => void;
  children: ReactNode;
}) {
  const { transport, server, onForgotten, onPairAgain } = props;
  const [link, setLink] = useState<Link>({ state: "up" });
  const [lastCallMs, setLastCallMs] = useState<number | null>(null);
  const [lastReachedAt, setLastReachedAt] = useState<number | null>(null);
  const [epoch, setEpoch] = useState(0);
  const [now, setNow] = useState(() => Date.now());
  const [projects, setProjectsState] = useState<ProjectRecord[] | null>(null);
  const [projectsAt, setProjectsAt] = useState<number | null>(null);
  const retrying = useRef(false);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const noteDown = useCallback((err: TransportError) => {
    if (!alive.current || !isDownKind(err.kind)) return;
    const kind = err.kind;
    setLink((prev) => {
      if (prev.state === "down" && prev.kind === kind) return { ...prev, message: err.message };
      return {
        state: "down",
        kind,
        message: err.message,
        attempt: 0,
        nextAt: kind === "unreachable" ? Date.now() + retryDelaySeconds(0) * 1000 : null,
      };
    });
  }, []);

  const call = useCallback(
    async <T,>(fn: (t: Transport) => Promise<T>): Promise<T> => {
      const t0 = performance.now();
      const finish = (reached: boolean) => {
        if (!alive.current) return;
        setLastCallMs(Math.max(0, Math.round(performance.now() - t0)));
        if (reached) {
          setLastReachedAt(Date.now());
          // Any HTTP answer shows that the server is reachable.
          setLink((prev) => (prev.state === "up" ? prev : { state: "up" }));
        }
      };
      try {
        const out = await fn(transport);
        finish(true);
        return out;
      } catch (e) {
        const err = asTransportError(e);
        if (isDownKind(err.kind)) {
          finish(false);
          noteDown(err);
        } else {
          finish(true);
        }
        throw err;
      }
    },
    [transport, noteDown],
  );

  const retry = useCallback(async () => {
    if (retrying.current) return;
    retrying.current = true;
    const t0 = performance.now();
    try {
      await transport.status();
      if (!alive.current) return;
      setLastCallMs(Math.max(0, Math.round(performance.now() - t0)));
      setLastReachedAt(Date.now());
      setLink({ state: "up" });
      setEpoch((e) => e + 1);
    } catch (e) {
      if (!alive.current) return;
      const err = asTransportError(e);
      setLastCallMs(Math.max(0, Math.round(performance.now() - t0)));
      if (err.kind === "unreachable") {
        setLink((prev) => {
          const attempt = prev.state === "down" && prev.kind === "unreachable" ? prev.attempt + 1 : 0;
          return {
            state: "down",
            kind: "unreachable",
            message: err.message,
            attempt,
            nextAt: Date.now() + retryDelaySeconds(attempt) * 1000,
          };
        });
      } else if (isDownKind(err.kind)) {
        setLink({ state: "down", kind: err.kind, message: err.message, attempt: 0, nextAt: null });
      } else {
        // The server answered, with a refusal that is no disconnect: the open page fetches again
        // and shows its own error line.
        setLastReachedAt(Date.now());
        setLink({ state: "up" });
        setEpoch((n) => n + 1);
      }
    } finally {
      retrying.current = false;
    }
  }, [transport]);

  const retryNow = useCallback(() => {
    void retry();
  }, [retry]);

  const downUnreachable = link.state === "down" && link.kind === "unreachable";
  const nextAt = link.state === "down" ? link.nextAt : null;

  // The only timer that calls: the retry of `status()` while the server is unreachable.
  useEffect(() => {
    if (!downUnreachable || nextAt === null) return;
    const id = setTimeout(() => {
      void retry();
    }, Math.max(0, nextAt - Date.now()));
    return () => clearTimeout(id);
  }, [downUnreachable, nextAt, retry]);

  // The countdown on screen. It makes no call, and it runs only while the server is unreachable.
  useEffect(() => {
    if (!downUnreachable) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [downUnreachable]);

  const setProjects = useCallback((list: ProjectRecord[]) => {
    setProjectsState(list);
    setProjectsAt(Date.now());
  }, []);

  const forget = useCallback(async () => {
    await transport.forget();
    onForgotten(server);
  }, [transport, onForgotten, server]);

  const value = useMemo<Connection>(
    () => ({
      server,
      link,
      lastCallMs,
      lastReachedAt,
      epoch,
      now,
      call,
      report: noteDown,
      retryNow,
      forget,
      pairAgain: onPairAgain,
      projects,
      projectsAt,
      setProjects,
    }),
    [server, link, lastCallMs, lastReachedAt, epoch, now, call, noteDown, retryNow, forget, onPairAgain, projects, projectsAt, setProjects],
  );

  return <Ctx.Provider value={value}>{props.children}</Ctx.Provider>;
}
